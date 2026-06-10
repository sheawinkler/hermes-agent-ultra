//! Hermes Agent — binary entry point.
//!
//! Initializes logging, parses CLI arguments, and dispatches to the
//! appropriate subcommand handler.

mod doctor;
mod gateway_handlers;
mod gateway_main;
mod gateway_process;
mod interactive_lock;
mod provenance;
mod route_learning;
mod session_resume;

use doctor::*;
use gateway_main::*;
use gateway_process::*;
use interactive_lock::*;
use provenance::*;
use route_learning::*;
use session_resume::*;

use hermes_cli::gateway_runtime_defaults;
use hermes_cli::startup_metrics::StartupMetrics;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::Aead;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use clap_complete::{Shell as CompletionShell, generate};
use hermes_agent::AgentLoop;
use hermes_auth::{
    AuthManager, FileTokenStore, OAuth2Endpoints, OAuthCredential, exchange_refresh_token,
};
use hermes_cli::App;
use hermes_cli::app::{
    async_tool_dispatch_for, bridge_tool_registry, build_agent_config, build_provider,
    provider_api_key_from_env,
};
use hermes_cli::auth::{
    ANTHROPIC_OAUTH_CLIENT_ID, ANTHROPIC_OAUTH_TOKEN_URL, AnthropicOAuthLoginOptions,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL, CodexDeviceCodeOptions, DEFAULT_CODEX_BASE_URL,
    DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS, DEFAULT_NOUS_CLIENT_ID, DEFAULT_NOUS_PORTAL_URL,
    DEFAULT_OPENAI_BASE_URL, GeminiOAuthLoginOptions, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    NousAuthState, NousDeviceCodeOptions, NousRuntimeCredentials,
    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, QWEN_OAUTH_CLIENT_ID, QWEN_OAUTH_TOKEN_URL,
    clear_provider_auth_state, discover_existing_anthropic_oauth, discover_existing_nous_oauth,
    discover_existing_openai_codex_oauth, discover_existing_openai_oauth,
    get_anthropic_oauth_status, get_gemini_oauth_auth_status, get_qwen_auth_status,
    login_anthropic_oauth, login_google_gemini_cli_oauth, login_nous_device_code,
    login_openai_codex_device_code, login_openai_device_code, read_provider_auth_state,
    resolve_gemini_oauth_runtime_credentials, resolve_nous_runtime_credentials,
    resolve_qwen_runtime_credentials, save_codex_auth_state, save_nous_auth_state,
    save_openai_auth_state, save_provider_auth_state,
};
use hermes_cli::cli::{Cli, CliCommand};
use hermes_cli::config_env::hydrate_env_from_config;
use hermes_cli::cron_delivery::GatewayCronDeliveryBackend;
use hermes_cli::model_switch::{
    cached_provider_catalog_status, curated_provider_slugs, normalize_provider_model,
    provider_catalog_entries, provider_model_ids,
};
use hermes_cli::providers::provider_capability_for;
use hermes_cli::runtime_tool_wiring::{
    wire_cron_scheduler_backend, wire_gateway_clarify_backend, wire_gateway_messaging_backend,
};
use hermes_cli::terminal_backend::build_terminal_backend;
use hermes_config::{
    ConfigError, GatewayConfig, PlatformConfig, UnauthorizedDmBehavior, apply_user_config_patch,
    hermes_home, load_config, load_user_config_file, save_config_yaml, state_dir,
    user_config_field_display, validate_config,
};
use hermes_core::AgentError;
use hermes_core::MessageRole;
use hermes_core::PlatformAdapter;
use hermes_core::init_global_clock;
use hermes_cron::{
    CronCompletionEvent, CronError, CronRunner, CronScheduler, FileJobPersistence,
    cron_scheduler_for_data_dir,
};
use hermes_gateway::gateway::GatewayConfig as RuntimeGatewayConfig;
use hermes_gateway::gateway::IncomingMessage as GatewayIncomingMessage;
use hermes_gateway::gateway::{DmAccessMode, GroupAccessMode, PlatformAccessPolicy};
use hermes_gateway::hooks::HookRegistry;
use hermes_gateway::platforms::api_server::{ApiInboundRequest, ApiServerAdapter, ApiServerConfig};
use hermes_gateway::platforms::bluebubbles::{BlueBubblesAdapter, BlueBubblesConfig};
use hermes_gateway::platforms::dingtalk::{DingTalkAdapter, DingTalkConfig};
use hermes_gateway::platforms::discord::{DiscordAdapter, DiscordConfig};
use hermes_gateway::platforms::email::{EmailAdapter, EmailConfig};
use hermes_gateway::platforms::feishu::{FeishuAdapter, FeishuConfig};
use hermes_gateway::platforms::homeassistant::{HomeAssistantAdapter, HomeAssistantConfig};
use hermes_gateway::platforms::matrix::{MatrixAdapter, MatrixConfig};
use hermes_gateway::platforms::mattermost::{MattermostAdapter, MattermostConfig};
use hermes_gateway::platforms::ntfy::{NtfyAdapter, NtfyConfig};
use hermes_gateway::platforms::qqbot::{QqBotAdapter, QqBotConfig};
use hermes_gateway::platforms::signal::{SignalAdapter, SignalConfig};
use hermes_gateway::platforms::slack::{SlackAdapter, SlackConfig};
use hermes_gateway::platforms::sms::{SmsAdapter, SmsConfig};
use hermes_gateway::platforms::telegram::{TelegramAdapter, TelegramConfig};
use hermes_gateway::platforms::webhook::{WebhookAdapter, WebhookConfig, WebhookPayload};
use hermes_gateway::platforms::wecom::{WeComAdapter, WeComConfig};
use hermes_gateway::platforms::wecom_callback::{
    WeComCallbackAdapter, WeComCallbackApp, WeComCallbackConfig,
};
use hermes_gateway::platforms::weixin::{WeChatAdapter, WeixinConfig};
use hermes_gateway::platforms::whatsapp::{WhatsAppAdapter, WhatsAppConfig, is_paired};
use hermes_gateway::tool_backends::ClarifyDispatcher;
use hermes_gateway::{DmManager, Gateway, GatewayRuntimeContext, SessionManager};
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_telemetry::init_telemetry_from_env;
use hermes_tools::{ToolRegistry, default_tool_policy_counters_path, load_tool_policy_counters};
use hmac::KeyInit as _;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

fn auth_error_message(err: &AgentError) -> Option<String> {
    match err {
        AgentError::LlmApi(msg)
        | AgentError::Config(msg)
        | AgentError::ToolExecution(msg)
        | AgentError::Gateway(msg)
        | AgentError::AuthFailed(msg) => Some(msg.to_ascii_lowercase()),
        _ => None,
    }
}

fn oneshot_auth_is_refreshable(message: &str) -> bool {
    message.contains("401")
        || message.contains("403")
        || message.contains("unauthorized")
        || message.contains("invalid token")
        || message.contains("token expired")
        || message.contains("authentication failed")
        || message.contains("invalid_grant")
        || message.contains("expired")
}

fn infer_oauth_provider_from_error_message(message: &str) -> Option<String> {
    if message.contains("portal.nousresearch.com")
        || message.contains("inference-api.nousresearch.com")
        || message.contains(" provider nous")
        || message.contains("nous:")
    {
        return Some("nous".to_string());
    }
    if message.contains("console.anthropic.com")
        || message.contains("claude.ai")
        || message.contains("anthropic")
    {
        return Some("anthropic".to_string());
    }
    if message.contains("chat.qwen.ai") || message.contains("dashscope") || message.contains("qwen")
    {
        return Some("qwen-oauth".to_string());
    }
    if message.contains("oauth2.googleapis.com")
        || message.contains("googleapis.com")
        || message.contains("gemini")
        || message.contains("google")
    {
        return Some("google-gemini-cli".to_string());
    }
    if message.contains("auth.openai.com")
        || message.contains("chatgpt.com")
        || message.contains("openai")
        || message.contains("codex")
    {
        if message.contains("codex") || message.contains("chatgpt.com") {
            return Some("openai-codex".to_string());
        }
        return Some("openai".to_string());
    }
    None
}

fn query_is_local_slash_command(query: &str) -> bool {
    query.trim_start().starts_with('/')
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "auto"
            )
        })
        .unwrap_or(false)
}

fn oneshot_should_use_app_runtime(query: &str) -> bool {
    !query_is_local_slash_command(query)
        && (env_truthy("HERMES_ONESHOT_APP_RUNTIME") || env_truthy("HERMES_QUORUM_AUTO_ARM"))
}

#[cfg(target_os = "windows")]
fn start_gateway_keepawake_guard() -> Option<keepawake::KeepAwake> {
    if !gateway_running_on_ac_power() {
        tracing::info!("gateway keep-awake skipped on Windows: system is on battery");
        return None;
    }
    match keepawake::Builder::default()
        .idle(true)
        .sleep(true)
        .reason("Hermes Gateway is running")
        .app_name("Hermes Gateway")
        .create()
    {
        Ok(guard) => {
            tracing::info!("gateway keep-awake guard enabled on Windows");
            Some(guard)
        }
        Err(err) => {
            tracing::warn!("gateway keep-awake unavailable on Windows: {err}");
            None
        }
    }
}

#[cfg(target_os = "windows")]
fn gateway_running_on_ac_power() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = SYSTEM_POWER_STATUS {
        ACLineStatus: 0,
        BatteryFlag: 0,
        BatteryLifePercent: 0,
        SystemStatusFlag: 0,
        BatteryLifeTime: 0,
        BatteryFullLifeTime: 0,
    };
    let ok = unsafe { GetSystemPowerStatus(&mut status) } != 0;
    if !ok {
        tracing::warn!("failed to read Windows power status; defaulting to battery mode");
        return false;
    }

    status.ACLineStatus == 1
}

#[cfg(not(target_os = "windows"))]
fn start_gateway_keepawake_guard() {}

fn print_app_oneshot_result(app: &App) {
    if let Some(reply) = app.messages.iter().rev().find_map(|message| {
        if message.role == MessageRole::Assistant {
            message
                .content
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        } else {
            None
        }
    }) {
        println!("{}", reply);
    }
}

async fn handle_local_slash_query(cli: Cli, query: &str) -> Result<bool, AgentError> {
    if !query_is_local_slash_command(query) {
        return Ok(false);
    }
    let mut app = App::new(cli).await?;
    app.handle_input(query).await?;
    Ok(true)
}

fn oneshot_auto_verify_oauth_provider(
    err: &AgentError,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Option<String> {
    let Some(message) = auth_error_message(err) else {
        return None;
    };

    if !oneshot_auth_is_refreshable(&message) {
        return None;
    }

    let mut candidates: Vec<String> = Vec::new();
    if let Some(raw_provider) = provider_override.map(str::trim).filter(|v| !v.is_empty()) {
        candidates.push(normalize_auth_provider(raw_provider));
    }
    if let Some(raw_model_provider) = model_override
        .and_then(|m| m.split_once(':').map(|(provider, _)| provider.trim()))
        .filter(|v| !v.is_empty())
    {
        candidates.push(normalize_auth_provider(raw_model_provider));
    }
    if let Some(from_message) = infer_oauth_provider_from_error_message(&message) {
        candidates.push(from_message);
    }

    let mut seen = HashSet::new();
    for candidate in candidates {
        let normalized = normalize_auth_provider(&candidate);
        if !seen.insert(normalized.clone()) {
            continue;
        }
        if provider_supports_oauth(&normalized) {
            return Some(normalized);
        }
    }
    None
}

fn main() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let status = std::thread::Builder::new()
        .name("hermes-ultra-main".into())
        .spawn(main_thread_entry)
        .expect("failed to spawn main thread")
        .join()
        .expect("main thread panicked");
    if let Err(code) = status {
        std::process::exit(code);
    }
}

fn main_thread_entry() -> Result<(), i32> {
    let (version, commit) = hermes_core::startup_commit_info();
    eprintln!(
        "[WARN] hermes-cli startup commit info: version={} commit={}",
        version, commit
    );

    if cfg!(debug_assertions) {
        if std::env::var("HERMES_CLI_PARSE_PROBE").ok().as_deref() == Some("1") {
            eprintln!("[probe] before Cli::try_parse()");
            let parse_result = Cli::try_parse();
            eprintln!("[probe] after Cli::try_parse()");
            match parse_result {
                Ok(_) => {
                    eprintln!("[probe] parse ok");
                    return Ok(());
                }
                Err(err) => err.exit(),
            }
        }
    }

    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("Error: failed to initialize async runtime: {}", err);
            return Err(1);
        }
    };
    runtime.block_on(async_main(cli));
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    Ok(())
}

async fn async_main(cli: Cli) {
    run(cli).await;
}

async fn run(cli: Cli) {
    if let Some(config_dir) = cli.config_dir.as_deref() {
        hermes_cli::env_vars::set_var("HERMES_HOME", config_dir);
    }
    let prior_home = std::env::var("HERMES_HOME").ok();
    let migrated_home = hermes_config::ensure_migrated_hermes_home(cli.config_dir.as_deref());
    hermes_cli::env_vars::set_var("HERMES_HOME", migrated_home.to_string_lossy().as_ref());
    log_legacy_home_env_hint(prior_home.as_deref(), &migrated_home);
    if cli.ignore_user_config {
        hermes_cli::env_vars::set_var("HERMES_IGNORE_USER_CONFIG", "1");
    }
    if cli.ignore_rules {
        hermes_cli::env_vars::set_var("HERMES_IGNORE_RULES", "1");
        hermes_cli::env_vars::set_var("HERMES_AGENT_SKIP_CONTEXT_FILES", "1");
    }
    if cli.accept_hooks {
        hermes_cli::env_vars::set_var("HERMES_ACCEPT_HOOKS", "1");
        hermes_agent::shell_hooks::set_process_accept_hooks(true);
    }
    let effective_command = cli.effective_command();
    let global_model_override = cli.model.clone();
    let global_provider_override = cli.provider.clone();
    let global_allow_tools_override = cli.allow_tools;

    // Initialize tracing
    init_tracing(
        cli.verbose,
        matches!(
            effective_command,
            CliCommand::Hermes | CliCommand::Resume { .. }
        ),
        matches!(effective_command, CliCommand::Gateway { .. }),
    );
    if let Err(err) = hydrate_provider_env_from_vault_for_cli(&cli).await {
        tracing::warn!("Secret-vault hydration skipped: {}", err);
    }
    if let Ok(cfg) = load_config(cli.config_dir.as_deref()) {
        init_global_clock(cfg.timezone.as_deref());
        let applied = hydrate_env_from_config(&cfg);
        tracing::trace!(
            applied_env_vars = applied,
            "Hydrated environment from config.yaml"
        );
    } else {
        init_global_clock(None);
    }
    let route_autotune_applied = apply_route_autotune_env_overrides(&hermes_state_root(&cli));
    if !route_autotune_applied.is_empty() {
        tracing::debug!(
            applied_env_vars = ?route_autotune_applied,
            "Hydrated environment from route-autotune overrides"
        );
    }

    tracing::debug!("Hermes Agent starting");

    if let Some(prompt) = cli.oneshot.clone() {
        match handle_local_slash_query(cli.clone(), &prompt).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(err) => {
                eprintln!("Error: {}", err);
                std::process::exit(1);
            }
        }
        if oneshot_should_use_app_runtime(&prompt) {
            let mut app = match App::new(cli.clone()).await {
                Ok(app) => app,
                Err(err) => {
                    eprintln!("Error: {}", err);
                    std::process::exit(1);
                }
            };
            if let Err(err) = app.handle_input(&prompt).await {
                eprintln!("Error: {}", err);
                std::process::exit(1);
            }
            print_app_oneshot_result(&app);
            return;
        }
        let mut result = hermes_cli::commands::handle_cli_chat(
            Some(prompt),
            None,
            false,
            global_model_override.clone(),
            global_provider_override.clone(),
            global_allow_tools_override,
        )
        .await;
        if let Err(err) = &result {
            if let Some(provider) = oneshot_auto_verify_oauth_provider(
                err,
                global_provider_override.as_deref(),
                global_model_override.as_deref(),
            ) {
                eprintln!(
                    "Detected OAuth auth failure for provider '{}' in one-shot mode; running `hermes-ultra auth verify {}` and retrying once...",
                    provider, provider
                );
                if let Err(verify_err) = run_auth(
                    cli.clone(),
                    Some("verify".to_string()),
                    Some(provider.clone()),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await
                {
                    eprintln!(
                        "Warning: automatic `auth verify {}` failed: {}",
                        provider, verify_err
                    );
                }
                result = hermes_cli::commands::handle_cli_chat(
                    Some(cli.oneshot.clone().unwrap_or_default()),
                    None,
                    false,
                    global_model_override.clone(),
                    global_provider_override.clone(),
                    global_allow_tools_override,
                )
                .await;
                if provider == "nous" {
                    if let Err(retry_err) = &result {
                        if oneshot_auto_verify_oauth_provider(
                            retry_err,
                            Some(provider.as_str()),
                            global_model_override.as_deref(),
                        )
                        .as_deref()
                            == Some("nous")
                        {
                            eprintln!(
                                "Nous OAuth still invalid; launching `hermes-ultra auth login nous` and retrying once..."
                            );
                            if let Err(login_err) = run_auth(
                                cli.clone(),
                                Some("login".to_string()),
                                Some("nous".to_string()),
                                None,
                                None,
                                None,
                                None,
                                false,
                            )
                            .await
                            {
                                eprintln!(
                                    "Warning: automatic `auth login nous` failed: {}",
                                    login_err
                                );
                            } else {
                                result = hermes_cli::commands::handle_cli_chat(
                                    Some(cli.oneshot.clone().unwrap_or_default()),
                                    None,
                                    false,
                                    global_model_override.clone(),
                                    global_provider_override.clone(),
                                    global_allow_tools_override,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        }
        if let Err(e) = result {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let result = match effective_command {
        CliCommand::Hermes => run_interactive(cli).await,
        CliCommand::Chat {
            query,
            preload_skill,
            yolo,
        } => {
            run_chat_command(
                cli,
                query,
                preload_skill,
                yolo,
                global_model_override.clone(),
                global_provider_override.clone(),
                global_allow_tools_override,
            )
            .await
        }
        CliCommand::Model { provider_model } => run_model(cli, provider_model).await,
        CliCommand::Tools {
            action,
            name,
            platform,
            summary,
        } => run_tools(cli, action, name, platform, summary).await,
        CliCommand::Config { action, key, value } => run_config(cli, action, key, value).await,
        CliCommand::Gateway {
            action,
            system,
            all,
            force,
            run_as_user,
            replace,
            dry_run,
            yes,
            deep,
        } => {
            run_gateway_command(
                cli,
                action,
                system,
                all,
                force,
                run_as_user,
                replace,
                dry_run,
                yes,
                deep,
            )
            .await
        }
        CliCommand::Setup { portal } => {
            if portal {
                run_portal(cli, Some("setup".to_string())).await
            } else {
                run_setup(cli).await
            }
        }
        CliCommand::Portal { action } => run_portal(cli, action).await,
        CliCommand::Doctor {
            deep,
            self_heal,
            snapshot,
            snapshot_path,
            bundle,
        } => run_doctor(cli, deep, self_heal, snapshot, snapshot_path, bundle).await,
        CliCommand::Update {
            check,
            yes,
            rollback,
            force,
            source,
            channel,
        } => run_update(check, yes, rollback, force, source, channel).await,
        CliCommand::EliteCheck { json, strict } => run_elite_check(cli, json, strict).await,
        CliCommand::VerifyProvenance {
            path,
            signature,
            strict,
            json,
        } => run_verify_provenance(cli, path, signature, strict, json).await,
        CliCommand::RotateProvenanceKey { json } => run_rotate_provenance_key(cli, json).await,
        CliCommand::RouteLearning { action, json } => run_route_learning(cli, action, json).await,
        CliCommand::RouteHealth { action, json } => run_route_health(cli, action, json).await,
        CliCommand::RouteAutotune {
            action,
            apply,
            strict,
            json,
        } => run_route_autotune(cli, action, apply, strict, json).await,
        CliCommand::IncidentPack {
            snapshot,
            output,
            json,
        } => run_incident_pack(cli, snapshot, output, json).await,
        CliCommand::Status => run_status(cli).await,
        CliCommand::Kanban { args } => run_kanban(args),
        CliCommand::Systems {
            action,
            topic,
            json,
            output,
            host,
            port,
            once,
        } => {
            hermes_cli::systems::handle_cli_systems(hermes_cli::systems::SystemsCliOptions {
                config_dir: cli.config_dir.clone(),
                action,
                topic,
                json_only: json,
                output,
                host,
                port,
                once,
            })
            .await
        }
        CliCommand::TeamsPipeline {
            action,
            id,
            limit,
            status,
            store_path,
            meeting_id,
            join_web_url,
            tenant_id,
            call_record_id,
            resource,
            notification_url,
            change_type,
            expiration,
            client_state,
            lifecycle_notification_url,
            latest_supported_tls_version,
            force_refresh,
            renew_within_hours,
            extend_hours,
            dry_run,
        } => {
            hermes_cli::teams_pipeline_cli::handle_cli_teams_pipeline(
                hermes_cli::teams_pipeline_cli::TeamsPipelineCliOptions {
                    config_dir: cli.config_dir.clone(),
                    action,
                    id,
                    limit,
                    status,
                    store_path,
                    meeting_id,
                    join_web_url,
                    tenant_id,
                    call_record_id,
                    resource,
                    notification_url,
                    change_type,
                    expiration,
                    client_state,
                    lifecycle_notification_url,
                    latest_supported_tls_version,
                    force_refresh,
                    renew_within_hours,
                    extend_hours,
                    dry_run,
                },
            )
            .await
        }
        CliCommand::Dashboard {
            host,
            port,
            no_open,
            insecure,
        } => run_dashboard(cli, host, port, no_open, insecure).await,
        CliCommand::Debug {
            action,
            url,
            lines,
            expire,
            local,
        } => run_debug(cli, action, url, lines, expire, local).await,
        CliCommand::Logs { lines, follow } => run_logs(cli, lines, follow).await,
        CliCommand::Profile {
            action,
            name,
            secondary,
            output,
            import_name,
            alias_name,
            remove,
            yes,
            clone,
            clone_all,
            clone_from,
            no_alias,
            no_skills,
        } => {
            run_profile_command(
                cli,
                action,
                name,
                secondary,
                output,
                import_name,
                alias_name,
                remove,
                yes,
                clone,
                clone_all,
                clone_from,
                no_alias,
                no_skills,
            )
            .await
        }
        CliCommand::Auth {
            action,
            provider,
            target,
            auth_type,
            label,
            api_key,
            qr,
        } => run_auth(cli, action, provider, target, auth_type, label, api_key, qr).await,
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
        } => hermes_cli::commands::skills::handle_cli_skills(action, name, extra).await,
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
        CliCommand::Memory {
            action,
            target,
            yes,
        } => hermes_cli::commands::handle_cli_memory(action, target, yes).await,
        CliCommand::Interest {
            action,
            mode,
            llm_on_session_end,
            rest,
        } => {
            hermes_cli::commands::handle_cli_interest(action, mode, llm_on_session_end, rest).await
        }
        CliCommand::Contribute {
            action,
            poi_only,
            skills_only,
            last_session,
            outbox_clear,
        } => {
            hermes_cli::commands::handle_cli_contribute(
                action,
                poi_only,
                skills_only,
                last_session,
                outbox_clear,
            )
            .await
        }
        CliCommand::Mcp {
            action,
            name,
            server,
            url,
            command,
            parallel_tools,
        } => {
            hermes_cli::commands::handle_cli_mcp(action, name, server, url, command, parallel_tools)
                .await
        }
        CliCommand::Sessions { action, id, name } => {
            hermes_cli::commands::handle_cli_sessions(action, id, name).await
        }
        CliCommand::Resume { session_id } => run_resume(cli, session_id).await,
        CliCommand::Insights { days, source } => {
            hermes_cli::commands::handle_cli_insights(days, source).await
        }
        CliCommand::Login { provider } => {
            run_auth(
                cli,
                Some("login".to_string()),
                provider,
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        CliCommand::Logout { provider } => {
            run_auth(
                cli,
                Some("logout".to_string()),
                provider,
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        CliCommand::Whatsapp { action } => hermes_cli::commands::handle_cli_whatsapp(action).await,
        CliCommand::Pairing {
            action,
            device_id,
            args,
        } => hermes_cli::commands::handle_cli_pairing(action, device_id, args).await,
        CliCommand::Claw { action } => hermes_cli::commands::handle_cli_claw(action).await,
        CliCommand::Acp { action } => hermes_cli::commands::handle_cli_acp(action).await,
        CliCommand::Backup { output } => hermes_cli::commands::handle_cli_backup(output).await,
        CliCommand::Import { path } => hermes_cli::commands::handle_cli_import(path).await,
        CliCommand::Version => hermes_cli::commands::handle_cli_version(),
        CliCommand::Cron {
            action,
            job_id,
            id,
            schedule,
            prompt,
            name,
            deliver,
            repeat,
            skills,
            add_skills,
            remove_skills,
            clear_skills,
            script,
            no_agent,
            agent,
            script_timeout_seconds,
            script_shell,
            all,
        } => {
            run_cron(
                cli,
                action,
                job_id,
                id,
                schedule,
                prompt,
                name,
                deliver,
                repeat,
                skills,
                add_skills,
                remove_skills,
                clear_skills,
                script,
                no_agent,
                agent,
                script_timeout_seconds,
                script_shell,
                all,
            )
            .await
        }
        CliCommand::Webhook {
            action,
            name,
            url,
            id,
            prompt,
            events,
            description,
            skills,
            deliver,
            deliver_chat_id,
            secret,
            deliver_only,
            payload,
        } => {
            run_webhook(
                cli,
                action,
                name,
                url,
                id,
                prompt,
                events,
                description,
                skills,
                deliver,
                deliver_chat_id,
                secret,
                deliver_only,
                payload,
            )
            .await
        }
        CliCommand::Dump { session, output } => run_dump(cli, session, output).await,
        CliCommand::Completion { shell } => run_completion(shell),
        CliCommand::Uninstall { yes } => run_uninstall(yes).await,
        CliCommand::Lumio { action, model } => run_lumio(action, model).await,
        CliCommand::Meeting {
            action,
            audio,
            title,
            mode,
            diarize,
        } => hermes_cli::commands::handle_cli_meeting(action, audio, title, mode, diarize).await,
        CliCommand::PluginExternal(raw) => {
            hermes_cli::commands::handle_cli_external_plugin_subcommand(raw).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run_chat_command(
    cli: Cli,
    query: Option<String>,
    preload_skill: Option<String>,
    yolo: bool,
    global_model_override: Option<String>,
    global_provider_override: Option<String>,
    global_allow_tools_override: bool,
) -> Result<(), AgentError> {
    if let Some(prompt) = query.clone() {
        match handle_local_slash_query(cli, &prompt).await {
            Ok(true) => Ok(()),
            Ok(false) => {
                hermes_cli::commands::handle_cli_chat(
                    query,
                    preload_skill,
                    yolo,
                    global_model_override,
                    global_provider_override,
                    global_allow_tools_override,
                )
                .await
            }
            Err(err) => Err(err),
        }
    } else {
        hermes_cli::commands::handle_cli_chat(
            query,
            preload_skill,
            yolo,
            global_model_override,
            global_provider_override,
            global_allow_tools_override,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_gateway_command(
    cli: Cli,
    action: Option<String>,
    system: bool,
    all: bool,
    force: bool,
    run_as_user: Option<String>,
    replace: bool,
    dry_run: bool,
    yes: bool,
    deep: bool,
) -> Result<(), AgentError> {
    run_gateway(
        cli,
        action,
        system,
        all,
        force,
        run_as_user,
        replace,
        dry_run,
        yes,
        deep,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_profile_command(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    secondary: Option<String>,
    output: Option<String>,
    import_name: Option<String>,
    alias_name: Option<String>,
    remove: bool,
    yes: bool,
    clone: bool,
    clone_all: bool,
    clone_from: Option<String>,
    no_alias: bool,
    no_skills: bool,
) -> Result<(), AgentError> {
    run_profile(
        cli,
        action,
        name,
        secondary,
        output,
        import_name,
        alias_name,
        remove,
        yes,
        clone,
        clone_all,
        clone_from,
        no_alias,
        no_skills,
    )
    .await
}

/// Initialize the tracing subscriber with env filter.
fn init_tracing(verbose: bool, interactive_tui: bool, gateway: bool) {
    let default = if interactive_tui {
        if verbose {
            "info,rustls=warn,hyper=warn,h2=warn"
        } else {
            "error,rustls=warn,hyper=warn,h2=warn"
        }
    } else if verbose {
        "debug,hermes_cron=debug,rustls=warn,hyper=warn,h2=warn"
    } else if gateway {
        // Gateway runs cron in-process; surface schedule/trigger/delivery at info without -v.
        "warn,hermes_cron=info,rustls=warn,hyper=warn,h2=warn"
    } else {
        "warn,rustls=warn,hyper=warn,h2=warn"
    };
    if interactive_tui
        && std::env::var("HERMES_TUI_ALLOW_STDERR_LOGS")
            .ok()
            .as_deref()
            != Some("1")
    {
        hermes_cli::env_vars::set_var("RUST_LOG", default);
    }
    init_telemetry_from_env("hermes-cli", default);
}

/// Run the interactive REPL (default command).
async fn run_interactive(cli: Cli) -> Result<(), AgentError> {
    let _session_lock = InteractiveSessionLockGuard::acquire(&hermes_state_root(&cli))?;
    let app = App::new(cli).await?;
    hermes_cli::tui::run(app).await
}

#[derive(Debug, Clone)]
struct ResumeSessionPayload {
    resolved_id: String,
    source_path: PathBuf,
    session_id: String,
    model: Option<String>,
    personality: Option<String>,
    messages: Vec<hermes_core::Message>,
}

async fn run_resume(cli: Cli, requested_session_id: Option<String>) -> Result<(), AgentError> {
    let _session_lock = InteractiveSessionLockGuard::acquire(&hermes_state_root(&cli))?;
    let requested = requested_session_id.as_deref();
    let payload = match load_resume_payload(&cli, requested) {
        Ok(payload) => payload,
        Err(err) if should_resume_fallback_to_fresh(requested, &err) => {
            let mut app = App::new(cli).await?;
            app.push_ui_assistant(
                "No saved sessions found yet. Started a fresh session; future turns will autosave for `resume`.",
            );
            return hermes_cli::tui::run(app).await;
        }
        Err(err) => return Err(err),
    };
    let mut app = App::new(cli).await?;

    if let Some(model) = payload.model.clone().filter(|m| !m.trim().is_empty()) {
        if model != app.current_model {
            app.switch_model(&model);
        } else {
            app.current_model = model;
        }
    }

    app.current_personality = payload
        .personality
        .clone()
        .filter(|name| !name.trim().is_empty());
    app.session_id = payload.session_id.clone();
    app.messages = payload.messages;
    app.ui_messages.clear();
    app.input_history.clear();
    app.history_index = 0;
    app.session_objective = extract_session_objective(&app.messages);
    app.push_ui_assistant(format!(
        "Resumed session `{}` from {} ({} messages).",
        payload.resolved_id,
        payload.source_path.display(),
        app.messages.len()
    ));

    hermes_cli::tui::run(app).await
}

fn load_resume_payload(
    cli: &Cli,
    requested: Option<&str>,
) -> Result<ResumeSessionPayload, AgentError> {
    let sessions_dir = hermes_state_root(cli).join("sessions");
    let (resolved_id, source_path) =
        resolve_resume_session_file_with_legacy_fallback(&sessions_dir, requested)?;
    let mut payload = parse_resume_payload_file(resolved_id, source_path)?;
    if is_latest_resume_request(requested) && payload.messages.is_empty() {
        if let Ok((fallback_id, fallback_path)) =
            resolve_latest_nonempty_session_file_with_legacy_fallback(&sessions_dir)
        {
            if fallback_path != payload.source_path {
                if let Ok(fallback_payload) =
                    parse_resume_payload_file(fallback_id.clone(), fallback_path.clone())
                {
                    tracing::info!(
                        "resume latest selected non-empty snapshot {} from {}",
                        fallback_id,
                        fallback_path.display()
                    );
                    payload = fallback_payload;
                }
            }
        }
    }
    Ok(payload)
}

fn parse_resume_payload_file(
    resolved_id: String,
    source_path: PathBuf,
) -> Result<ResumeSessionPayload, AgentError> {
    let raw = std::fs::read_to_string(&source_path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read session file {}: {}",
            source_path.display(),
            e
        ))
    })?;
    let doc: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        AgentError::Config(format!(
            "Failed to parse session file {}: {}",
            source_path.display(),
            e
        ))
    })?;

    let info = doc.get("session_info");
    let session_id = info
        .and_then(|v| v.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| resolved_id.clone());
    let model = info
        .and_then(|v| v.get("model"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    let personality = info
        .and_then(|v| v.get("personality"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.get("personality")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    let messages_value = doc
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            AgentError::Config(format!(
                "Session file {} does not contain a valid `messages` array.",
                source_path.display()
            ))
        })?;

    let messages = parse_resume_messages(messages_value);

    Ok(ResumeSessionPayload {
        resolved_id,
        source_path,
        session_id,
        model,
        personality,
        messages,
    })
}

fn legacy_session_dirs() -> Vec<PathBuf> {
    hermes_config::legacy_hermes_home_candidates()
}

fn log_legacy_home_env_hint(prior_home: Option<&str>, migrated_home: &Path) {
    let migrated = migrated_home.to_string_lossy();
    let Some(prior) = prior_home.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if prior != migrated.as_ref() {
        tracing::info!(
            prior_hermes_home = prior,
            effective_hermes_home = migrated.as_ref(),
            "HERMES_HOME was remapped to the fresh ultra home for this process; legacy data is not copied — update your user environment variable if you want new shells to match"
        );
    }
}

fn resolve_resume_session_file_with_legacy_fallback(
    sessions_dir: &Path,
    requested: Option<&str>,
) -> Result<(String, PathBuf), AgentError> {
    match resolve_resume_session_file(sessions_dir, requested) {
        Ok(found) => Ok(found),
        Err(primary_err) => {
            for legacy_dir in legacy_session_dirs() {
                if legacy_dir == sessions_dir || !legacy_dir.exists() {
                    continue;
                }
                if let Ok(found) = resolve_resume_session_file(&legacy_dir, requested) {
                    return Ok(found);
                }
            }
            Err(primary_err)
        }
    }
}

fn resolve_latest_nonempty_session_file_with_legacy_fallback(
    sessions_dir: &Path,
) -> Result<(String, PathBuf), AgentError> {
    match resolve_latest_nonempty_session_file(sessions_dir) {
        Ok(found) => Ok(found),
        Err(primary_err) => {
            for legacy_dir in legacy_session_dirs() {
                if legacy_dir == sessions_dir || !legacy_dir.exists() {
                    continue;
                }
                if let Ok(found) = resolve_latest_nonempty_session_file(&legacy_dir) {
                    return Ok(found);
                }
            }
            Err(primary_err)
        }
    }
}

fn is_latest_resume_request(requested: Option<&str>) -> bool {
    let requested = requested.unwrap_or("latest").trim();
    requested.is_empty() || requested.eq_ignore_ascii_case("latest")
}

fn should_resume_fallback_to_fresh(requested: Option<&str>, err: &AgentError) -> bool {
    if !is_latest_resume_request(requested) {
        return false;
    }
    match err {
        AgentError::Config(msg) | AgentError::Io(msg) => {
            msg.contains("No saved sessions found") || msg.contains("No sessions directory found")
        }
        _ => false,
    }
}

fn resolve_latest_nonempty_session_file(
    sessions_dir: &Path,
) -> Result<(String, PathBuf), AgentError> {
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    let rd = std::fs::read_dir(sessions_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read sessions directory {}: {}",
            sessions_dir.display(),
            e
        ))
    })?;
    for entry in rd.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if !path.extension().map(|ext| ext == "json").unwrap_or(false) {
            continue;
        }
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((path, modified));
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    // Prefer canonical snapshots: file stem == session_info.session_id.
    for (path, _) in candidates {
        if let Some(summary) = session_file_summary(&path) {
            if summary.message_count > 0 && summary.canonical {
                let resolved_id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "latest".to_string());
                return Ok((resolved_id, path));
            }
        }
    }
    Err(AgentError::Config(format!(
        "No non-empty saved sessions found in {}.",
        sessions_dir.display()
    )))
}

#[derive(Debug, Clone, Default)]
struct SessionFileSummary {
    message_count: usize,
    canonical: bool,
}

fn session_file_summary(path: &Path) -> Option<SessionFileSummary> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return None;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return None;
    };
    let message_count = doc
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let stem = path
        .file_stem()
        .and_then(|v| v.to_str())
        .map(str::trim)
        .unwrap_or_default();
    let session_id = doc
        .get("session_info")
        .and_then(|v| v.get("session_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let canonical =
        !stem.is_empty() && !session_id.is_empty() && stem.eq_ignore_ascii_case(session_id);
    Some(SessionFileSummary {
        message_count,
        canonical,
    })
}

fn resolve_resume_session_file(
    sessions_dir: &Path,
    requested: Option<&str>,
) -> Result<(String, PathBuf), AgentError> {
    let req = requested.unwrap_or("latest").trim();
    if req.is_empty() || req.eq_ignore_ascii_case("latest") {
        let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        let rd = std::fs::read_dir(sessions_dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to read sessions directory {}: {}",
                sessions_dir.display(),
                e
            ))
        })?;
        for entry in rd.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if !path.extension().map(|ext| ext == "json").unwrap_or(false) {
                continue;
            }
            let modified = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            candidates.push((path, modified));
        }
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        // 1) newest canonical non-empty snapshot
        for (path, _) in &candidates {
            if let Some(summary) = session_file_summary(path) {
                if summary.canonical && summary.message_count > 0 {
                    let resolved_id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "latest".to_string());
                    return Ok((resolved_id, path.clone()));
                }
            }
        }
        // 2) newest canonical snapshot (may be startup stub)
        for (path, _) in &candidates {
            if let Some(summary) = session_file_summary(path) {
                if summary.canonical {
                    let resolved_id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "latest".to_string());
                    return Ok((resolved_id, path.clone()));
                }
            }
        }
        let Some((path, _)) = candidates.into_iter().next() else {
            return Err(AgentError::Config(format!(
                "No saved sessions found in {}.",
                sessions_dir.display()
            )));
        };
        let resolved_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "latest".to_string());
        return Ok((resolved_id, path));
    }

    if req.contains('/') || req.contains('\\') {
        return Err(AgentError::Config(
            "Session ID should be a file stem, not a path.".into(),
        ));
    }

    let mut path = sessions_dir.join(req);
    if path.extension().is_none() {
        path.set_extension("json");
    }
    if !path.exists() {
        return Err(AgentError::Config(format!(
            "Session '{}' not found at {}.",
            req,
            path.display()
        )));
    }

    let resolved_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| req.to_string());
    Ok((resolved_id, path))
}

fn parse_resume_messages(items: &[serde_json::Value]) -> Vec<hermes_core::Message> {
    let mut messages = Vec::new();
    for item in items {
        let role = item
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user")
            .trim()
            .to_ascii_lowercase();
        let content = item
            .get("content")
            .or_else(|| item.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match role.as_str() {
            "system" => {
                if !content.is_empty() {
                    messages.push(hermes_core::Message::system(content));
                }
            }
            "assistant" => {
                if let Some(tool_calls_val) = item.get("tool_calls") {
                    if let Ok(tool_calls) =
                        serde_json::from_value::<Vec<hermes_core::ToolCall>>(tool_calls_val.clone())
                    {
                        messages.push(hermes_core::Message::assistant_with_tool_calls(
                            if content.is_empty() {
                                None
                            } else {
                                Some(content.clone())
                            },
                            tool_calls,
                        ));
                        continue;
                    }
                }
                if !content.is_empty() {
                    messages.push(hermes_core::Message::assistant(content));
                }
            }
            "tool" => {
                let tool_call_id = item
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool_call");
                if !content.is_empty() {
                    messages.push(hermes_core::Message::tool_result(tool_call_id, content));
                }
            }
            _ => {
                if !content.is_empty() {
                    messages.push(hermes_core::Message::user(content));
                }
            }
        }
    }
    messages
}

fn extract_session_objective(messages: &[hermes_core::Message]) -> Option<String> {
    const SESSION_OBJECTIVE_PREFIX: &str = "[SESSION_OBJECTIVE] ";
    messages.iter().find_map(|message| {
        if message.role != MessageRole::System {
            return None;
        }
        let content = message.content.as_deref()?.trim();
        content
            .strip_prefix(SESSION_OBJECTIVE_PREFIX)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// Handle `hermes model [provider:model]`.
async fn run_model(cli: Cli, provider_model: Option<String>) -> Result<(), AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match provider_model {
        Some(pm) => {
            let normalized = normalize_provider_model(&pm)?;
            let cfg_path = hermes_state_root(&cli).join("config.yaml");
            let mut disk =
                load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
            disk.model = Some(normalized.clone());
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!("Model switched to: {}", normalized);
            println!("Persisted default model in {}.", cfg_path.display());
        }
        None => {
            let current = config.model.as_deref().unwrap_or("gpt-4o");
            println!("Current model: {}", current);

            // List providers with merged models.dev-aware previews.
            let providers = curated_provider_slugs();
            let entries = provider_catalog_entries(&providers, 3).await;
            println!("\nAvailable providers:");
            if entries.is_empty() {
                println!("  openai       — OpenAI (gpt-4o, gpt-4o-mini, ...)");
                println!("  anthropic    — Anthropic (claude-3-5-sonnet, claude-3-opus, ...)");
                println!("  openrouter   — OpenRouter (multi-provider routing)");
                println!("  stepfun      — Step Plan / StepFun (step-3.5-flash, ...)");
            } else {
                for entry in entries {
                    let preview = entry.models.join(", ");
                    let suffix = if entry.total_models > entry.models.len() {
                        format!(" (+{} more)", entry.total_models - entry.models.len())
                    } else {
                        String::new()
                    };
                    let mut caps = Vec::new();
                    if let Some(cap) = provider_capability_for(&entry.provider) {
                        if cap.oauth_supported {
                            caps.push("oauth");
                        }
                        if cap.models_dev_merged {
                            caps.push("models.dev");
                        }
                        if cap.managed_tools_supported {
                            caps.push("managed-tools");
                        }
                    }
                    if let Some(cache_status) = cached_provider_catalog_status(&entry.provider) {
                        if cache_status.verified {
                            if let Some(age) = cache_status.age_secs {
                                caps.push(if age < 60 {
                                    "signed-cache:fresh"
                                } else {
                                    "signed-cache"
                                });
                            } else {
                                caps.push("signed-cache");
                            }
                        } else {
                            caps.push("cache-unverified");
                        }
                    }
                    let cap_suffix = if caps.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", caps.join(", "))
                    };
                    println!(
                        "  {:<12} — {}{}{}",
                        entry.provider, preview, suffix, cap_suffix
                    );
                }
            }
            println!("\nUsage: hermes model <provider>:<model>");
        }
    }
    Ok(())
}

/// Handle `hermes tools [action]`.
async fn run_tools(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    platform: Option<String>,
    summary: bool,
) -> Result<(), AgentError> {
    let runtime_config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&runtime_config);
    let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&registry, terminal_backend, skill_provider);
    let tools = registry.list_tools();
    let base: PathBuf = cli
        .config_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(hermes_home);
    let cfg_path = base.join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        None | Some("list") => {
            let enabled = &disk.tools_config.enabled;
            let disabled = &disk.tools_config.disabled;
            if summary {
                println!(
                    "Tool summary (platform={}):",
                    platform.as_deref().unwrap_or("cli")
                );
                println!(
                    "  enabled: {}",
                    if enabled.is_empty() {
                        "(none)".to_string()
                    } else {
                        enabled.join(", ")
                    }
                );
                println!(
                    "  disabled: {}",
                    if disabled.is_empty() {
                        "(none)".to_string()
                    } else {
                        disabled.join(", ")
                    }
                );
                return Ok(());
            }

            if tools.is_empty() {
                println!("No tools registered (tools are loaded at runtime).");
            } else {
                println!("Registered tools ({}):", tools.len());
                for tool in &tools {
                    let state = if disabled.iter().any(|t| t == &tool.name) {
                        "disabled"
                    } else {
                        "enabled"
                    };
                    println!("  • {} [{}] — {}", tool.name, state, tool.description);
                }
                println!("\nScope: {}", platform.as_deref().unwrap_or("cli"));
            }
        }
        Some("enable") => {
            let tool_name = name.ok_or_else(|| {
                AgentError::Config("tools enable: usage `hermes tools enable <name>`".into())
            })?;
            if !disk.tools_config.enabled.iter().any(|t| t == &tool_name) {
                disk.tools_config.enabled.push(tool_name.clone());
            }
            disk.tools_config.disabled.retain(|t| t != &tool_name);
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!(
                "Enabled tool '{}' for platform '{}'.",
                tool_name,
                platform.as_deref().unwrap_or("cli")
            );
        }
        Some("disable") => {
            let tool_name = name.ok_or_else(|| {
                AgentError::Config("tools disable: usage `hermes tools disable <name>`".into())
            })?;
            if !disk.tools_config.disabled.iter().any(|t| t == &tool_name) {
                disk.tools_config.disabled.push(tool_name.clone());
            }
            disk.tools_config.enabled.retain(|t| t != &tool_name);
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!(
                "Disabled tool '{}' for platform '{}'.",
                tool_name,
                platform.as_deref().unwrap_or("cli")
            );
        }
        Some("setup") => {
            run_tools_setup_wizard(&cli).await?;
        }
        Some(other) => {
            println!(
                "Unknown tools action: {}. Use 'list', 'enable', 'disable', or 'setup'.",
                other
            );
        }
    }
    Ok(())
}

async fn run_tools_setup_wizard(cli: &Cli) -> Result<(), AgentError> {
    let runtime_config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&runtime_config);
    let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&registry, terminal_backend, skill_provider);
    let mut tools = registry.list_tools();
    if tools.is_empty() {
        println!("No tools registered (tools are loaded at runtime).");
        return Ok(());
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    let explicit_enabled = !disk.tools_config.enabled.is_empty();

    let mut pre_selected: HashSet<usize> = HashSet::new();
    let mut rows: Vec<String> = Vec::with_capacity(tools.len());
    let summarize = |text: &str| -> String {
        let flattened: String = text
            .chars()
            .map(|ch| match ch {
                '\n' | '\r' | '\t' => ' ',
                c if c.is_control() => ' ',
                c => c,
            })
            .collect();
        let compact = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
        let max_chars = 120usize;
        if compact.chars().count() <= max_chars {
            compact
        } else {
            let mut out = compact
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>();
            out.push('…');
            out
        }
    };
    for (idx, tool) in tools.iter().enumerate() {
        let currently_enabled = if explicit_enabled {
            disk.tools_config
                .enabled
                .iter()
                .any(|name| name == &tool.name)
        } else {
            !disk
                .tools_config
                .disabled
                .iter()
                .any(|name| name == &tool.name)
        };
        if currently_enabled {
            pre_selected.insert(idx);
        }
        rows.push(format!(
            "{:<24} {:<8} {}",
            tool.name,
            if currently_enabled {
                "enabled"
            } else {
                "disabled"
            },
            summarize(&tool.description)
        ));
    }

    let result = hermes_cli::curses_checklist(
        "Select enabled tools",
        &rows,
        &pre_selected,
        Some(&|selected| format!("{} selected", selected.len())),
    );
    if !result.confirmed {
        println!("Tools setup cancelled.");
        return Ok(());
    }

    let mut enabled_known: Vec<String> = result
        .selected
        .iter()
        .copied()
        .filter_map(|idx| tools.get(idx).map(|t| t.name.clone()))
        .collect();
    enabled_known.sort();
    enabled_known.dedup();
    let enabled_known_set: HashSet<String> = enabled_known.iter().cloned().collect();

    let mut known_tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    known_tool_names.sort();
    known_tool_names.dedup();
    let known_tool_set: HashSet<String> = known_tool_names.iter().cloned().collect();

    let mut disabled_known: Vec<String> = known_tool_names
        .into_iter()
        .filter(|name| !enabled_known_set.contains(name))
        .collect();
    disabled_known.sort();
    disabled_known.dedup();

    // Preserve unknown/custom tool keys while replacing known-tool state.
    disk.tools_config
        .enabled
        .retain(|name| !known_tool_set.contains(name));
    disk.tools_config
        .disabled
        .retain(|name| !known_tool_set.contains(name));
    disk.tools_config.enabled.extend(enabled_known.clone());
    disk.tools_config.disabled.extend(disabled_known.clone());
    disk.tools_config.enabled.sort();
    disk.tools_config.enabled.dedup();
    disk.tools_config.disabled.sort();
    disk.tools_config.disabled.dedup();

    save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
    if enabled_known_set.contains("computer_use") {
        ensure_computer_use_runtime_ready().await?;
    }
    println!(
        "Updated tools setup: {} enabled, {} disabled (config: {}).",
        enabled_known.len(),
        disabled_known.len(),
        cfg_path.display()
    );
    Ok(())
}

async fn ensure_computer_use_runtime_ready() -> Result<(), AgentError> {
    println!("\nComputer Use runtime check:");
    let mut driver_present = which::which("cua-driver").is_ok();
    if !driver_present {
        println!("  - cua-driver not found on PATH.");
        let do_install = prompt_yes_no("Install cua-driver-rs now?", true).await?;
        if do_install {
            let installed = install_cua_driver_rs_windows().await;
            if installed {
                driver_present = which::which("cua-driver").is_ok();
            }
        } else {
            println!("  - skipped installation.");
        }
    }
    if !driver_present {
        println!("  - computer_use will run in fallback capture-only mode.");
        println!("  - to enable full actions, install cua-driver-rs and reopen setup.");
        return Ok(());
    }

    if cfg!(windows) {
        match hermes_tools::ensure_cua_driver_daemon_running().await {
            Ok(()) => println!("  - Computer Use desktop service is ready."),
            Err(err) => {
                println!("  - Computer Use desktop service could not start: {err}");
                println!("  - Try reinstalling via `hermes tools` → Computer Use.");
            }
        }
    }

    let list_tools_ok = run_cua_driver_health_command(&["list-tools"]).await;
    let list_windows_ok = run_cua_driver_health_command(&["list_windows"]).await;
    if list_tools_ok && list_windows_ok {
        println!("  - cua-driver health check passed (list-tools + list_windows).");
    } else {
        println!("  - cua-driver health check has warnings (Computer Use may still work).");
    }
    Ok(())
}

async fn install_cua_driver_rs_windows() -> bool {
    if !cfg!(windows) {
        println!("  - auto-install currently implemented for Windows only.");
        return false;
    }
    let ps = which::which("powershell")
        .or_else(|_| which::which("pwsh"))
        .ok();
    let Some(ps_bin) = ps else {
        println!("  - PowerShell not found; cannot auto-install cua-driver-rs.");
        return false;
    };

    println!("  - installing cua-driver-rs via official installer...");
    let script = "irm https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.ps1 | iex";
    let output = tokio::process::Command::new(ps_bin)
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            println!("  - cua-driver-rs install command succeeded.");
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let snippet = stderr.lines().take(3).collect::<Vec<_>>().join(" | ");
            println!("  - install failed: {}", snippet);
            false
        }
        Err(err) => {
            println!("  - install command error: {}", err);
            false
        }
    }
}

async fn run_cua_driver_health_command(args: &[&str]) -> bool {
    let output = tokio::process::Command::new("cua-driver")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            println!("  - cua-driver {}: ok", args.join(" "));
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let snippet = stderr.lines().take(2).collect::<Vec<_>>().join(" | ");
            println!("  - cua-driver {}: failed ({})", args.join(" "), snippet);
            false
        }
        Err(err) => {
            println!("  - cua-driver {}: error ({})", args.join(" "), err);
            false
        }
    }
}

async fn run_optional_setup_sections(
    cli: &Cli,
    current_config: &GatewayConfig,
) -> Result<(), AgentError> {
    let items = vec![
        "Messaging platforms (gateway setup wizard)".to_string(),
        "Tools (interactive enable/disable checklist)".to_string(),
        "Memory backend setup (initialize MEMORY.md/USER.md)".to_string(),
        "Sentrux MCP setup (quality workflow backend)".to_string(),
    ];
    let mut pre_selected: HashSet<usize> = HashSet::new();
    if current_config.platforms.values().any(|p| p.enabled) {
        pre_selected.insert(0);
    }
    if !current_config.tools_config.enabled.is_empty()
        || !current_config.tools_config.disabled.is_empty()
    {
        pre_selected.insert(1);
    }
    let memory_root = hermes_home();
    let memory_enabled = !memory_root.join(".memory_disabled").exists();
    let memory_ready = memory_enabled
        && memory_root.join("memories").join("MEMORY.md").exists()
        && memory_root.join("memories").join("USER.md").exists();
    if memory_ready {
        pre_selected.insert(2);
    }
    if current_config
        .mcp_servers
        .iter()
        .any(|entry| entry.name.eq_ignore_ascii_case("sentrux"))
    {
        pre_selected.insert(3);
    }

    let selected = hermes_cli::curses_checklist(
        "Optional setup sections",
        &items,
        &pre_selected,
        Some(&|choice| {
            if choice.is_empty() {
                "none selected".to_string()
            } else {
                format!("{} selected", choice.len())
            }
        }),
    );
    if !selected.confirmed {
        println!("Skipped optional setup sections.");
        return Ok(());
    }
    let mut order: Vec<usize> = selected.selected.iter().copied().collect();
    order.sort_unstable();
    for idx in order {
        match idx {
            0 => {
                println!("\nOpening gateway setup...");
                run_gateway_setup(cli).await?;
            }
            1 => {
                println!("\nOpening tools setup...");
                run_tools_setup_wizard(cli).await?;
            }
            2 => {
                println!("\nOpening memory setup...");
                hermes_cli::commands::handle_cli_memory(Some("setup".to_string()), None, false)
                    .await?;
            }
            3 => {
                println!("\nOpening sentrux MCP setup...");
                hermes_cli::commands::handle_cli_mcp(
                    Some("sentrux-setup".to_string()),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await?;
            }
            _ => {}
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

/// Handle `hermes gateway [action]`.
#[allow(clippy::too_many_arguments)]
async fn run_gateway(
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
            let pid_path = gateway_pid_path_for_cli(&hermes_state_root(&cli));
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
            drop(_p2);

            let _p3 = _metrics.phase("pid_check");
            let pid_path = gateway_pid_path_for_cli(&hermes_state_root(&cli));
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
            let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_cli::gateway_inbound_wiring::wire_gateway_inbound_vision(
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
            let pid_path = gateway_pid_path_for_cli(&hermes_state_root(&cli));
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
            let pid_path = gateway_pid_path_for_cli(&hermes_state_root(&cli));
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

fn enabled_flag(platform: Option<&PlatformConfig>) -> &'static str {
    if platform.map(|p| p.enabled).unwrap_or(false) {
        "enabled"
    } else {
        "disabled"
    }
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

fn gateway_platform_menu_label(
    entry: &GatewayPlatformEntry,
    platform: Option<&PlatformConfig>,
) -> String {
    let status = if entry.key == "whatsapp" {
        hermes_cli::whatsapp_wizard::whatsapp_gateway_menu_status(platform)
    } else if gateway_platform_is_configured(entry.key, platform) {
        "configured"
    } else {
        "not configured"
    };
    format!("{} {}  ({status})", entry.emoji, entry.label)
}

async fn run_gateway_setup(cli: &Cli) -> Result<(), AgentError> {
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

        let pick = hermes_cli::prompt_choice(
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

async fn run_api_server_inbound_loop(
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

async fn run_webhook_inbound_loop(gateway: Arc<Gateway>, mut rx: mpsc::Receiver<WebhookPayload>) {
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

async fn run_gateway_incoming_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<GatewayIncomingMessage>,
    platform: &'static str,
) {
    while let Some(incoming) = rx.recv().await {
        spawn_gateway_route(gateway.clone(), incoming, platform);
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

/// Default auth provider: CLI arg, then `HERMES_AUTH_DEFAULT_PROVIDER`, then `nous`.
///
/// Set `HERMES_AUTH_DEFAULT_PROVIDER=telegram` if you primarily use the Telegram gateway.
fn resolve_auth_provider(provider: Option<String>) -> String {
    if let Some(raw) = provider.filter(|s| !s.trim().is_empty()) {
        return normalize_auth_provider(&raw);
    }

    if let Ok(pool) = std::env::var("HERMES_AUTH_PROVIDER_POOL") {
        for item in pool.split(',') {
            let item = item.trim();
            if !item.is_empty() {
                return normalize_auth_provider(item);
            }
        }
    }

    let raw = std::env::var("HERMES_AUTH_DEFAULT_PROVIDER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| infer_default_auth_provider_from_config())
        .unwrap_or_else(|| "nous".to_string());
    normalize_auth_provider(&raw)
}

fn infer_default_auth_provider_from_config() -> Option<String> {
    let cfg = load_config(None).ok()?;
    let model = cfg.model?;
    let provider = model
        .split_once(':')
        .map(|(provider, _)| provider.trim())
        .filter(|provider| !provider.is_empty())?;
    Some(provider.to_string())
}

fn normalize_auth_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "wechat" | "wx" => "weixin".to_string(),
        "qq" => "qqbot".to_string(),
        "tg" => "telegram".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "codex" => "openai-codex".to_string(),
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi" => "kimi-coding".to_string(),
        "minimax-cn" | "minimax_cn" | "minimax-china" => "minimax-cn".to_string(),
        "dashscope" | "aliyun" | "alibaba-cloud" => "alibaba".to_string(),
        "alibaba_coding" | "alibaba-coding" | "alibaba_coding_plan" => {
            "alibaba-coding-plan".to_string()
        }
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "opencode" | "zen" => "opencode-zen".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        "aigateway" | "vercel" | "vercel-ai-gateway" => "ai-gateway".to_string(),
        "x-ai" | "x.ai" | "grok" => "xai".to_string(),
        "glm" | "z-ai" | "z.ai" | "zhipu" => "zai".to_string(),
        "nim" | "nvidia-nim" | "build-nvidia" | "nemotron" => "nvidia".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "api-server" => "api_server".to_string(),
        "home-assistant" => "homeassistant".to_string(),
        "wecom-callback" => "wecom_callback".to_string(),
        "mm" => "mattermost".to_string(),
        "github-copilot" => "copilot".to_string(),
        other => other.to_string(),
    }
}

fn gateway_platform_provider_key(provider: &str) -> Option<&'static str> {
    match provider {
        "discord" => Some("discord"),
        "slack" => Some("slack"),
        "matrix" => Some("matrix"),
        "mattermost" => Some("mattermost"),
        "signal" => Some("signal"),
        "whatsapp" => Some("whatsapp"),
        "dingtalk" => Some("dingtalk"),
        "feishu" => Some("feishu"),
        "wecom" => Some("wecom"),
        "wecom_callback" => Some("wecom_callback"),
        "qqbot" | "qq" => Some("qqbot"),
        "bluebubbles" => Some("bluebubbles"),
        "email" => Some("email"),
        "sms" => Some("sms"),
        "homeassistant" => Some("homeassistant"),
        "webhook" => Some("webhook"),
        "api_server" => Some("api_server"),
        _ => None,
    }
}

fn normalize_secret_provider(provider: &str) -> String {
    let p = provider.trim().to_ascii_lowercase();
    match p.as_str() {
        "github-copilot" => "copilot".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "codex" => "openai-codex".to_string(),
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "moonshot" | "kimi" => "kimi-coding".to_string(),
        "aigateway" | "vercel" | "vercel-ai-gateway" => "ai-gateway".to_string(),
        "opencode" | "zen" => "opencode-zen".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "x-ai" | "x.ai" | "grok" => "xai".to_string(),
        "glm" | "z-ai" | "z.ai" | "zhipu" => "zai".to_string(),
        "nim" | "nvidia-nim" | "build-nvidia" | "nemotron" => "nvidia".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "dashscope" | "aliyun" | "alibaba-cloud" => "alibaba".to_string(),
        "alibaba_coding" | "alibaba-coding" | "alibaba_coding_plan" => {
            "alibaba-coding-plan".to_string()
        }
        _ => p,
    }
}

fn secret_provider_aliases(provider: &str) -> Vec<String> {
    match normalize_secret_provider(provider).as_str() {
        "anthropic" => vec![
            "anthropic".to_string(),
            "claude".to_string(),
            "claude-code".to_string(),
        ],
        "moonshot" | "kimi" | "kimi-coding" => vec![
            "kimi-coding".to_string(),
            "kimi".to_string(),
            "moonshot".to_string(),
        ],
        "kimi-coding-cn" => vec!["kimi-coding-cn".to_string()],
        "stepfun" => vec!["stepfun".to_string(), "step".to_string()],
        "copilot" => vec!["copilot".to_string(), "github-copilot".to_string()],
        "openai-codex" => vec!["openai-codex".to_string(), "codex".to_string()],
        "google-gemini-cli" => vec![
            "google-gemini-cli".to_string(),
            "gemini-cli".to_string(),
            "gemini-oauth".to_string(),
        ],
        "zai" => vec![
            "zai".to_string(),
            "glm".to_string(),
            "z-ai".to_string(),
            "z.ai".to_string(),
        ],
        "xai" => vec![
            "xai".to_string(),
            "x-ai".to_string(),
            "x.ai".to_string(),
            "grok".to_string(),
        ],
        "nvidia" => vec![
            "nvidia".to_string(),
            "nvidia-nim".to_string(),
            "nim".to_string(),
        ],
        "huggingface" => vec!["huggingface".to_string(), "hf".to_string()],
        "ai-gateway" => vec!["ai-gateway".to_string(), "aigateway".to_string()],
        "opencode-zen" => vec!["opencode-zen".to_string(), "opencode".to_string()],
        "kilocode" => vec!["kilocode".to_string(), "kilo".to_string()],
        "ollama-local" => vec!["ollama-local".to_string(), "ollama".to_string()],
        "llama-cpp" => vec![
            "llama-cpp".to_string(),
            "llama.cpp".to_string(),
            "llamacpp".to_string(),
        ],
        "vllm" => vec!["vllm".to_string(), "ollvm".to_string(), "llvm".to_string()],
        "mlx" => vec!["mlx".to_string(), "mlx-lm".to_string()],
        "apple-ane" => vec![
            "apple-ane".to_string(),
            "ane".to_string(),
            "apple-neural-engine".to_string(),
        ],
        "sglang" => vec!["sglang".to_string()],
        "tgi" => vec!["tgi".to_string(), "text-generation-inference".to_string()],
        p => vec![p.to_string()],
    }
}

fn provider_env_var(provider: &str) -> Option<&'static str> {
    match normalize_secret_provider(provider).as_str() {
        "openai" => Some("HERMES_OPENAI_API_KEY"),
        "openai-codex" => Some("HERMES_OPENAI_CODEX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "google-gemini-cli" => Some("HERMES_GEMINI_OAUTH_API_KEY"),
        "gemini" => Some("GOOGLE_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "qwen" | "alibaba" => Some("DASHSCOPE_API_KEY"),
        "alibaba-coding-plan" => Some("ALIBABA_CODING_PLAN_API_KEY"),
        "qwen-oauth" => Some("HERMES_QWEN_OAUTH_API_KEY"),
        "moonshot" | "kimi" | "kimi-coding" => Some("KIMI_API_KEY"),
        "kimi-coding-cn" => Some("KIMI_CN_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "minimax-cn" => Some("MINIMAX_CN_API_KEY"),
        "stepfun" => Some("STEPFUN_API_KEY"),
        "nous" => Some("NOUS_API_KEY"),
        "copilot" => Some("GITHUB_COPILOT_TOKEN"),
        "ai-gateway" => Some("AI_GATEWAY_API_KEY"),
        "arcee" => Some("ARCEEAI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "huggingface" => Some("HF_TOKEN"),
        "kilocode" => Some("KILOCODE_API_KEY"),
        "nvidia" => Some("NVIDIA_API_KEY"),
        "ollama-cloud" => Some("OLLAMA_API_KEY"),
        "ollama-local" => Some("OLLAMA_LOCAL_API_KEY"),
        "llama-cpp" => Some("LLAMA_CPP_API_KEY"),
        "vllm" => Some("VLLM_API_KEY"),
        "mlx" => Some("MLX_API_KEY"),
        "apple-ane" => Some("APPLE_ANE_API_KEY"),
        "sglang" => Some("SGLANG_API_KEY"),
        "tgi" => Some("TGI_API_KEY"),
        "opencode-go" => Some("OPENCODE_GO_API_KEY"),
        "opencode-zen" => Some("OPENCODE_ZEN_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "xiaomi" => Some("XIAOMI_API_KEY"),
        "zai" => Some("GLM_API_KEY"),
        _ => None,
    }
}

fn provider_supports_oauth(provider: &str) -> bool {
    let normalized = normalize_auth_provider(provider);
    hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(normalized.as_str()))
}

fn resolve_auth_type_for_provider(provider: &str, requested: Option<&str>) -> String {
    if let Some(raw) = requested.map(str::trim).filter(|v| !v.is_empty()) {
        return raw.replace('-', "_").to_ascii_lowercase();
    }
    if provider_supports_oauth(provider) {
        "oauth".to_string()
    } else {
        "api_key".to_string()
    }
}

fn parse_rfc3339_utc(value: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    value
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn parse_unix_millis_utc(value: Option<i64>) -> Option<chrono::DateTime<chrono::Utc>> {
    value.and_then(chrono::DateTime::from_timestamp_millis)
}

fn secret_vault_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("auth").join("tokens.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AuthPoolEntry {
    id: String,
    label: String,
    auth_type: String,
    source: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_status_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error_code: Option<u16>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct AuthPoolStore {
    #[serde(default)]
    providers: std::collections::BTreeMap<String, Vec<AuthPoolEntry>>,
}

fn auth_pool_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("auth").join("pool.json")
}

fn load_auth_pool_store(path: &Path) -> Result<AuthPoolStore, AgentError> {
    if !path.exists() {
        return Ok(AuthPoolStore::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw).map_err(|e| AgentError::Config(format!("parse pool: {}", e)))
}

fn save_auth_pool_store(path: &Path, store: &AuthPoolStore) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw = serde_json::to_string_pretty(store).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn resolve_pool_target(entries: &[AuthPoolEntry], target: &str) -> Option<usize> {
    if let Ok(index) = target.parse::<usize>() {
        if index >= 1 && index <= entries.len() {
            return Some(index - 1);
        }
    }
    if let Some((idx, _)) = entries.iter().enumerate().find(|(_, e)| e.id == target) {
        return Some(idx);
    }
    entries.iter().position(|e| e.label == target)
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
    let manager = AuthManager::new(store.clone());
    let mut hydrated_nous_from_vault = false;

    if let Some((_provider, token)) = lookup_secret_from_vault(&store, "nous").await {
        hermes_cli::env_vars::set_var("NOUS_API_KEY", token);
        hydrated_nous_from_vault = true;
    }

    if !hydrated_nous_from_vault {
        match resolve_nous_runtime_credentials(
            false,
            true,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        )
        .await
        {
            Ok(creds) => {
                hermes_cli::env_vars::set_var("NOUS_API_KEY", creds.api_key.clone());
                if !creds.base_url.trim().is_empty() {
                    hermes_cli::env_vars::set_var(
                        "NOUS_INFERENCE_BASE_URL",
                        creds.base_url.clone(),
                    );
                }
                let expires_at = parse_rfc3339_utc(creds.expires_at.as_deref());
                let _ = manager
                    .save_credential(OAuthCredential {
                        provider: "nous".to_string(),
                        access_token: creds.api_key,
                        refresh_token: creds.refresh_token,
                        token_type: creds.token_type,
                        scope: creds.scope,
                        expires_at,
                    })
                    .await;
            }
            Err(err) => {
                tracing::debug!("Nous runtime credential refresh skipped: {}", err);
            }
        }
    }

    let env_bindings = [
        ("HERMES_OPENAI_API_KEY", "openai"),
        ("OPENAI_API_KEY", "openai"),
        ("HERMES_OPENAI_CODEX_API_KEY", "openai-codex"),
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("ANTHROPIC_TOKEN", "anthropic"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "anthropic"),
        ("HERMES_GEMINI_OAUTH_API_KEY", "google-gemini-cli"),
        ("GOOGLE_API_KEY", "gemini"),
        ("GEMINI_API_KEY", "gemini"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("ALIBABA_CODING_PLAN_API_KEY", "alibaba-coding-plan"),
        ("HERMES_QWEN_OAUTH_API_KEY", "qwen-oauth"),
        ("KIMI_API_KEY", "kimi-coding"),
        ("KIMI_CODING_API_KEY", "kimi-coding"),
        ("KIMI_CN_API_KEY", "kimi-coding-cn"),
        ("MOONSHOT_API_KEY", "kimi-coding"),
        ("MINIMAX_API_KEY", "minimax"),
        ("MINIMAX_CN_API_KEY", "minimax-cn"),
        ("STEPFUN_API_KEY", "stepfun"),
        ("NOUS_API_KEY", "nous"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
        ("AI_GATEWAY_API_KEY", "ai-gateway"),
        ("ARCEEAI_API_KEY", "arcee"),
        ("ARCEE_API_KEY", "arcee"),
        ("DEEPSEEK_API_KEY", "deepseek"),
        ("HF_TOKEN", "huggingface"),
        ("KILOCODE_API_KEY", "kilocode"),
        ("NVIDIA_API_KEY", "nvidia"),
        ("OLLAMA_API_KEY", "ollama-cloud"),
        ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
        ("LLAMA_CPP_API_KEY", "llama-cpp"),
        ("VLLM_API_KEY", "vllm"),
        ("MLX_API_KEY", "mlx"),
        ("APPLE_ANE_API_KEY", "apple-ane"),
        ("SGLANG_API_KEY", "sglang"),
        ("TGI_API_KEY", "tgi"),
        ("OPENCODE_GO_API_KEY", "opencode-go"),
        ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
        ("XAI_API_KEY", "xai"),
        ("XIAOMI_API_KEY", "xiaomi"),
        ("GLM_API_KEY", "zai"),
        ("ZAI_API_KEY", "zai"),
        ("Z_AI_API_KEY", "zai"),
    ];

    for (env_var, provider) in env_bindings {
        let env_present = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty());
        if let Some(current) = env_present {
            if provider_supports_oauth(provider) {
                if let Some((_provider, secret)) = lookup_secret_from_vault(&store, provider).await
                {
                    if secret.trim() != current.trim() {
                        hermes_cli::env_vars::set_var(env_var, secret);
                    }
                }
            }
            continue;
        }
        if let Some((_provider, secret)) = lookup_secret_from_vault(&store, provider).await {
            hermes_cli::env_vars::set_var(env_var, secret);
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
    provider == "weixin"
}

fn is_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn secret_stdout_allowed() -> bool {
    std::env::var("HERMES_ALLOW_SECRET_STDOUT")
        .ok()
        .is_some_and(|v| is_truthy(&v))
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

async fn qqbot_app_id_from_env_or_prompt(existing: Option<&str>) -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("QQ_APP_ID") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Some(current) = existing {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter QQBot app_id (QQ_APP_ID): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("qqbot app_id prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let app_id = line.trim().to_string();
    if app_id.is_empty() {
        return Err(AgentError::Config(
            "QQBot app_id cannot be empty (set QQ_APP_ID or input manually)".to_string(),
        ));
    }
    Ok(app_id)
}

async fn qqbot_client_secret_from_env_or_prompt(
    existing: Option<&str>,
) -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("QQ_CLIENT_SECRET") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Some(current) = existing {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter QQBot client_secret (QQ_CLIENT_SECRET): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("qqbot client_secret prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let secret = line.trim().to_string();
    if secret.is_empty() {
        return Err(AgentError::Config(
            "QQBot client_secret cannot be empty (set QQ_CLIENT_SECRET or input manually)"
                .to_string(),
        ));
    }
    Ok(secret)
}

fn qqbot_portal_host_from_disk(disk: &hermes_config::GatewayConfig) -> String {
    if let Some(cfg) = disk.platforms.get("qqbot") {
        for key in ["portal_host", "qq_portal_host"] {
            if let Some(v) = cfg.extra.get(key).and_then(|v| v.as_str()) {
                let s = v.trim();
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    if let Ok(v) = std::env::var("QQ_PORTAL_HOST") {
        let s = v.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    "q.qq.com".to_string()
}

fn qqbot_onboard_endpoints_from_disk(disk: &hermes_config::GatewayConfig) -> (String, String) {
    let mut create_path = "/lite/create_bind_task".to_string();
    let mut poll_path = "/lite/poll_bind_result".to_string();

    if let Some(cfg) = disk.platforms.get("qqbot") {
        for key in ["onboard_create_path", "qr_create_path"] {
            if let Some(v) = cfg.extra.get(key).and_then(|v| v.as_str()) {
                let s = v.trim();
                if !s.is_empty() {
                    create_path = s.to_string();
                    break;
                }
            }
        }
        for key in ["onboard_poll_path", "qr_poll_path"] {
            if let Some(v) = cfg.extra.get(key).and_then(|v| v.as_str()) {
                let s = v.trim();
                if !s.is_empty() {
                    poll_path = s.to_string();
                    break;
                }
            }
        }
    }

    if let Ok(v) = std::env::var("QQ_ONBOARD_CREATE_PATH") {
        let s = v.trim();
        if !s.is_empty() {
            create_path = s.to_string();
        }
    }
    if let Ok(v) = std::env::var("QQ_ONBOARD_POLL_PATH") {
        let s = v.trim();
        if !s.is_empty() {
            poll_path = s.to_string();
        }
    }

    (create_path, poll_path)
}

fn qqbot_generate_bind_key_base64() -> String {
    use rand::TryRng;
    let mut key = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut key)
        .expect("rng failed");
    BASE64_STANDARD.encode(key)
}

fn qqbot_extract_string(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
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

fn qqbot_extract_i64(v: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(raw) = v.get(*key) {
            if let Some(parsed) = raw.as_i64() {
                return Some(parsed);
            }
            if let Some(parsed) = raw.as_str().and_then(|s| s.trim().parse::<i64>().ok()) {
                return Some(parsed);
            }
        }
    }
    None
}

fn qqbot_decrypt_secret(encrypted_base64: &str, key_base64: &str) -> Result<String, AgentError> {
    let key_bytes = BASE64_STANDARD.decode(key_base64.trim()).map_err(|e| {
        AgentError::Config(format!("qqbot qr decrypt: invalid bind key base64: {e}"))
    })?;
    if key_bytes.len() != 32 {
        return Err(AgentError::Config(format!(
            "qqbot qr decrypt: expected 32-byte key, got {}",
            key_bytes.len()
        )));
    }
    let encrypted_bytes = BASE64_STANDARD
        .decode(encrypted_base64.trim())
        .map_err(|e| {
            AgentError::Config(format!("qqbot qr decrypt: invalid encrypted secret: {e}"))
        })?;
    if encrypted_bytes.len() < 29 {
        return Err(AgentError::Config(
            "qqbot qr decrypt: encrypted payload too short".to_string(),
        ));
    }
    let nonce = aes_gcm::Nonce::from_slice(&encrypted_bytes[..12]);
    let cipher = <Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key_bytes)
        .map_err(|e| AgentError::Config(format!("qqbot qr decrypt: cipher init failed: {e}")))?;
    let plaintext = cipher
        .decrypt(nonce, &encrypted_bytes[12..])
        .map_err(|_| AgentError::Config("qqbot qr decrypt: decrypt failed".to_string()))?;
    String::from_utf8(plaintext)
        .map_err(|e| AgentError::Config(format!("qqbot qr decrypt: invalid utf-8: {e}")))
}

fn qqbot_connect_url(task_id: &str) -> String {
    format!(
        "https://q.qq.com/qqbot/openclaw/connect.html?task_id={}&_wv=2&source=hermes",
        urlencoding::encode(task_id.trim())
    )
}

fn qqbot_api_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("HermesAgentUltra/qqbot-onboard"),
    );
    headers
}

fn qqbot_join_https_url(host: &str, path: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    let path = path.trim();
    if path.starts_with('/') {
        format!("https://{}{}", host, path)
    } else {
        format!("https://{}/{}", host, path)
    }
}

async fn qqbot_create_bind_task(
    client: &reqwest::Client,
    portal_host: &str,
    create_path: &str,
    key_base64: &str,
) -> Result<String, AgentError> {
    let url = qqbot_join_https_url(portal_host, create_path);
    let resp = client
        .post(url)
        .headers(qqbot_api_headers())
        .json(&serde_json::json!({ "key": key_base64 }))
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("qqbot create_bind_task request failed: {e}")))?;
    let status = resp.status();
    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AgentError::Config(format!("qqbot create_bind_task parse failed: {e}")))?;
    if !status.is_success() {
        return Err(AgentError::Config(format!(
            "qqbot create_bind_task failed ({}): {}",
            status, payload
        )));
    }
    let retcode = qqbot_extract_i64(&payload, &["retcode"]).unwrap_or(-1);
    if retcode != 0 {
        let msg = qqbot_extract_string(&payload, &["msg", "message"])
            .unwrap_or_else(|| "create_bind_task returned non-zero retcode".to_string());
        return Err(AgentError::Config(format!(
            "qqbot create_bind_task retcode={retcode}: {msg}"
        )));
    }
    let task_id = payload
        .get("data")
        .and_then(|v| qqbot_extract_string(v, &["task_id"]))
        .ok_or_else(|| {
            AgentError::Config("qqbot create_bind_task missing data.task_id".to_string())
        })?;
    Ok(task_id)
}

async fn qqbot_poll_bind_result(
    client: &reqwest::Client,
    portal_host: &str,
    poll_path: &str,
    task_id: &str,
) -> Result<(i64, String, String, String), AgentError> {
    let url = qqbot_join_https_url(portal_host, poll_path);
    let resp = client
        .post(url)
        .headers(qqbot_api_headers())
        .json(&serde_json::json!({ "task_id": task_id }))
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("qqbot poll_bind_result request failed: {e}")))?;
    let status = resp.status();
    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AgentError::Config(format!("qqbot poll_bind_result parse failed: {e}")))?;
    if !status.is_success() {
        return Err(AgentError::Config(format!(
            "qqbot poll_bind_result failed ({}): {}",
            status, payload
        )));
    }
    let retcode = qqbot_extract_i64(&payload, &["retcode"]).unwrap_or(-1);
    if retcode != 0 {
        let msg = qqbot_extract_string(&payload, &["msg", "message"])
            .unwrap_or_else(|| "poll_bind_result returned non-zero retcode".to_string());
        return Err(AgentError::Config(format!(
            "qqbot poll_bind_result retcode={retcode}: {msg}"
        )));
    }
    let data = payload.get("data").cloned().unwrap_or_default();
    let status = qqbot_extract_i64(&data, &["status"]).unwrap_or_default();
    let app_id = qqbot_extract_string(&data, &["bot_appid", "app_id"]).unwrap_or_default();
    let encrypted_secret =
        qqbot_extract_string(&data, &["bot_encrypt_secret", "encrypt_secret"]).unwrap_or_default();
    let user_openid = qqbot_extract_string(&data, &["user_openid"]).unwrap_or_default();
    Ok((status, app_id, encrypted_secret, user_openid))
}

async fn qqbot_qr_login_flow(
    portal_host: &str,
    create_path: &str,
    poll_path: &str,
    timeout_seconds: u64,
) -> Result<(String, String, String), AgentError> {
    const ONBOARD_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    const MAX_REFRESHES: usize = 3;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AgentError::Io(format!("qqbot onboard client init failed: {e}")))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds);

    for refresh_idx in 0..=MAX_REFRESHES {
        let bind_key = qqbot_generate_bind_key_base64();
        let task_id = qqbot_create_bind_task(&client, portal_host, create_path, &bind_key).await?;
        let connect_url = qqbot_connect_url(&task_id);

        println!();
        println!("QQBot QR setup URL:");
        println!("  {}", connect_url);
        println!("Scan the URL with QQ on your phone.");
        render_qr_to_terminal(&connect_url);
        println!();

        loop {
            if std::time::Instant::now() >= deadline {
                return Err(AgentError::Timeout(format!(
                    "qqbot qr login timed out after {timeout_seconds}s"
                )));
            }
            match qqbot_poll_bind_result(&client, portal_host, poll_path, &task_id).await {
                Ok((status, app_id, encrypted_secret, user_openid)) => match status {
                    2 => {
                        if app_id.trim().is_empty() || encrypted_secret.trim().is_empty() {
                            return Err(AgentError::Config(
                                "qqbot qr confirmed but payload missing app_id/encrypted_secret"
                                    .to_string(),
                            ));
                        }
                        let client_secret = qqbot_decrypt_secret(&encrypted_secret, &bind_key)?;
                        return Ok((app_id, client_secret, user_openid));
                    }
                    3 => {
                        if refresh_idx >= MAX_REFRESHES {
                            return Err(AgentError::Timeout(format!(
                                "qqbot qr expired too many times (max {})",
                                MAX_REFRESHES
                            )));
                        }
                        println!(
                            "QQBot QR code expired, refreshing... ({}/{})",
                            refresh_idx + 1,
                            MAX_REFRESHES
                        );
                        break;
                    }
                    _ => {}
                },
                Err(_) => {}
            }
            tokio::time::sleep(ONBOARD_POLL_INTERVAL).await;
        }
    }
    Err(AgentError::Timeout(
        "qqbot qr login exhausted refresh retries".to_string(),
    ))
}

const WECOM_QR_GENERATE_URL: &str = "https://work.weixin.qq.com/ai/qc/generate";
const WECOM_QR_QUERY_URL: &str = "https://work.weixin.qq.com/ai/qc/query_result";
const WECOM_QR_CODE_PAGE: &str = "https://work.weixin.qq.com/ai/qc/gen?source=hermes&scode=";

fn wecom_qr_page_url(scode: &str) -> String {
    format!(
        "{}{}",
        WECOM_QR_CODE_PAGE,
        urlencoding::encode(scode.trim())
    )
}

async fn wecom_bot_id_from_env_or_prompt(existing: Option<&str>) -> Result<String, AgentError> {
    if let Some(v) = existing.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(v.to_string());
    }
    if let Ok(v) = std::env::var("WECOM_BOT_ID") {
        let s = v.trim();
        if !s.is_empty() {
            return Ok(s.to_string());
        }
    }
    let v = prompt_line("WeCom AI Bot bot_id (WECOM_BOT_ID): ").await?;
    let s = v.trim();
    if s.is_empty() {
        return Err(AgentError::Config(
            "WeCom bot_id is required (set WECOM_BOT_ID or enter at prompt)".to_string(),
        ));
    }
    Ok(s.to_string())
}

async fn wecom_secret_from_env_or_prompt(existing: Option<&str>) -> Result<String, AgentError> {
    if let Some(v) = existing.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(v.to_string());
    }
    if let Ok(v) = std::env::var("WECOM_SECRET") {
        let s = v.trim();
        if !s.is_empty() {
            return Ok(s.to_string());
        }
    }
    let v = prompt_line("WeCom AI Bot secret (WECOM_SECRET): ").await?;
    let s = v.trim();
    if s.is_empty() {
        return Err(AgentError::Config(
            "WeCom secret is required (set WECOM_SECRET or enter at prompt)".to_string(),
        ));
    }
    Ok(s.to_string())
}

async fn wecom_qr_login_flow(timeout_seconds: u64) -> Result<(String, String), AgentError> {
    const WECOM_QR_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AgentError::Io(format!("wecom qr client init failed: {e}")))?;

    print!("  Connecting to WeCom...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let generate_url = format!("{WECOM_QR_GENERATE_URL}?source=hermes");
    let raw = client
        .get(&generate_url)
        .header("User-Agent", "HermesAgent/1.0")
        .send()
        .await
        .map_err(|e| {
            println!(" failed: {e}");
            AgentError::Io(format!("wecom qr generate request: {e}"))
        })?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| {
            println!(" failed: {e}");
            AgentError::Io(format!("wecom qr generate parse: {e}"))
        })?;

    let data = raw.get("data").cloned().unwrap_or_default();
    let scode = weixin_extract_string(&data, &["scode"]).ok_or_else(|| {
        println!(" failed: unexpected response format");
        AgentError::Config("wecom qr response missing scode".to_string())
    })?;
    let auth_url = weixin_extract_string(&data, &["auth_url"]).ok_or_else(|| {
        println!(" failed: unexpected response format");
        AgentError::Config("wecom qr response missing auth_url".to_string())
    })?;

    println!(" done.");
    println!();
    render_qr_to_terminal(&auth_url);
    let page_url = wecom_qr_page_url(&scode);
    println!("\n  Scan the QR code above, or open this URL directly:\n  {page_url}");
    println!();
    print!("  Fetching configuration results...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds);
    let query_url = format!(
        "{WECOM_QR_QUERY_URL}?scode={}",
        urlencoding::encode(scode.trim())
    );

    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client
            .get(&query_url)
            .header("User-Agent", "HermesAgent/1.0")
            .send()
            .await
        {
            if let Ok(result) = resp.json::<serde_json::Value>().await {
                let result_data = result.get("data").cloned().unwrap_or_default();
                let status = weixin_extract_string(&result_data, &["status"])
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                print!(".");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                if status == "success" {
                    println!();
                    let bot_info = result_data.get("bot_info").cloned().unwrap_or_default();
                    let bot_id =
                        weixin_extract_string(&bot_info, &["botid", "bot_id"]).unwrap_or_default();
                    let secret = weixin_extract_string(&bot_info, &["secret"]).unwrap_or_default();
                    if !bot_id.is_empty() && !secret.is_empty() {
                        return Ok((bot_id, secret));
                    }
                    return Err(AgentError::Config(
                        "wecom qr scan reported success but bot credentials were incomplete"
                            .to_string(),
                    ));
                }
            }
        }
        tokio::time::sleep(WECOM_QR_POLL_INTERVAL).await;
    }

    println!();
    Err(AgentError::Timeout(format!(
        "wecom qr login timed out after {timeout_seconds}s"
    )))
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
    use qrcode::QrCode;

    let code = match QrCode::new(data.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            println!("  (QR code generation failed: {e})");
            println!("  Please open the URL above in your browser to scan.");
            return;
        }
    };

    let colors = code.to_colors();
    let width = code.width();

    // Use Unicode half-block characters for compact rendering
    // Upper half = top row, lower half = bottom row
    // ▀ (U+2580) = top black, bottom white
    // ▄ (U+2584) = top white, bottom black
    // █ (U+2588) = both black
    // ' ' = both white

    // Add quiet zone (white border)
    let quiet = 2;
    let total_w = width + quiet * 2;

    // Print rows two at a time using half-block characters
    let mut row = 0usize;
    while row < width + quiet * 2 {
        let mut line = String::new();
        line.push_str("  "); // indent
        for col in 0..total_w {
            let top_dark =
                if row >= quiet && row < width + quiet && col >= quiet && col < width + quiet {
                    colors[(row - quiet) * width + (col - quiet)] == qrcode::Color::Dark
                } else {
                    false
                };
            let bot_dark = if row + 1 >= quiet
                && row + 1 < width + quiet
                && col >= quiet
                && col < width + quiet
            {
                colors[(row + 1 - quiet) * width + (col - quiet)] == qrcode::Color::Dark
            } else {
                false
            };

            match (top_dark, bot_dark) {
                (true, true) => line.push('█'),
                (true, false) => line.push('▀'),
                (false, true) => line.push('▄'),
                (false, false) => line.push(' '),
            }
        }
        println!("{line}");
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

async fn print_auth_status_matrix(cli: &Cli, manager: &AuthManager) -> Result<(), AgentError> {
    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let disk = load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;

    println!("Auth status matrix:");
    println!("-------------------");

    let mut llm_providers = hermes_cli::providers::known_providers();
    llm_providers.sort_unstable();
    llm_providers.dedup();
    for provider in llm_providers {
        let env_present = provider_api_key_from_env(provider).is_some()
            || (provider == "copilot"
                && std::env::var("GITHUB_COPILOT_TOKEN")
                    .ok()
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false));
        let store_present = manager.get_access_token(provider).await?.is_some();
        let auth_state_present = if provider_supports_oauth(provider) {
            read_provider_auth_state(provider)?.is_some()
        } else {
            false
        };
        let (present, source) = if env_present {
            (true, "env")
        } else if store_present {
            (true, "token_store")
        } else if auth_state_present {
            (true, "auth_json")
        } else {
            (false, "none")
        };
        println!(
            "  - {:<16} present={} source={} oauth_state_present={}",
            provider, present, source, auth_state_present
        );
    }

    for provider in [
        "telegram",
        "weixin",
        "discord",
        "slack",
        "qqbot",
        "wecom_callback",
    ] {
        let (enabled, cfg_token) = disk
            .platforms
            .get(provider)
            .map(|p| (p.enabled, platform_token_or_extra(p).is_some()))
            .unwrap_or((false, false));
        let env_present = match provider {
            "telegram" => std::env::var("TELEGRAM_BOT_TOKEN")
                .ok()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
            "weixin" => std::env::var("WEIXIN_TOKEN")
                .ok()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
            "qqbot" => {
                std::env::var("QQ_APP_ID")
                    .ok()
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
                    && std::env::var("QQ_CLIENT_SECRET")
                        .ok()
                        .map(|v| !v.trim().is_empty())
                        .unwrap_or(false)
            }
            _ => false,
        };
        let (present, source) = if env_present {
            (true, "env")
        } else if cfg_token {
            (true, "config")
        } else {
            (false, "none")
        };
        println!(
            "  - {:<16} present={} source={} enabled={}",
            provider, present, source, enabled
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthVerifyOutcome {
    Valid,
    ValidRefreshed,
    Unverified,
    Missing,
    Expired,
    RefreshFailed,
}

impl AuthVerifyOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::ValidRefreshed => "valid_refreshed",
            Self::Unverified => "unverified",
            Self::Missing => "missing",
            Self::Expired => "expired",
            Self::RefreshFailed => "refresh_failed",
        }
    }

    fn is_success(self) -> bool {
        matches!(self, Self::Valid | Self::ValidRefreshed | Self::Unverified)
    }
}

#[derive(Debug, Clone)]
struct AuthVerifyResult {
    provider: String,
    outcome: AuthVerifyOutcome,
    source: String,
    credential_present: bool,
    oauth_state_present: bool,
    expires_at: Option<String>,
    detail: Option<String>,
}

fn auth_verify_source(env_present: bool, store_present: bool, auth_state_present: bool) -> String {
    if env_present {
        "env".to_string()
    } else if store_present {
        "token_store".to_string()
    } else if auth_state_present {
        "auth_json".to_string()
    } else {
        "none".to_string()
    }
}

fn oauth_refresh_config_for_provider(provider: &str) -> Option<(String, String)> {
    let token_url = match provider {
        "openai" => std::env::var("HERMES_OPENAI_OAUTH_TOKEN_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                std::env::var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or_else(|| CODEX_OAUTH_TOKEN_URL.to_string()),
        "openai-codex" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| CODEX_OAUTH_TOKEN_URL.to_string()),
        "anthropic" => std::env::var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| ANTHROPIC_OAUTH_TOKEN_URL.to_string()),
        _ => return None,
    };
    let client_id = match provider {
        "openai" => std::env::var("HERMES_OPENAI_OAUTH_CLIENT_ID")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                std::env::var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or_else(|| CODEX_OAUTH_CLIENT_ID.to_string()),
        "openai-codex" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| CODEX_OAUTH_CLIENT_ID.to_string()),
        "anthropic" => std::env::var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| ANTHROPIC_OAUTH_CLIENT_ID.to_string()),
        _ => return None,
    };
    Some((token_url, client_id))
}

async fn refresh_oauth_store_credential(
    provider: &str,
    current: &OAuthCredential,
) -> Result<OAuthCredential, AgentError> {
    let refresh_token = current
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(format!(
                "OAuth refresh token missing for provider '{}'",
                provider
            ))
        })?;
    let (token_url, client_id) = oauth_refresh_config_for_provider(provider).ok_or_else(|| {
        AgentError::AuthFailed(format!(
            "OAuth refresh not configured for provider '{}'",
            provider
        ))
    })?;
    let endpoints = OAuth2Endpoints {
        authorize_url: "http://127.0.0.1/oauth/authorize-unused".to_string(),
        token_url,
        client_id,
        redirect_uri: "http://127.0.0.1/oauth/callback-unused".to_string(),
        scopes: Vec::new(),
    };
    let mut refreshed = exchange_refresh_token(provider, &endpoints, refresh_token).await?;
    refreshed.provider = provider.to_string();
    Ok(refreshed)
}

async fn ensure_openai_oauth_credential(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<Option<OAuthCredential>, AgentError> {
    if let Some(existing) = token_store.get(provider).await {
        return Ok(Some(existing));
    }
    let imported = if provider == "openai" {
        discover_existing_openai_oauth()?
    } else {
        discover_existing_openai_codex_oauth()?
    };
    let Some(imported) = imported else {
        return Ok(None);
    };
    let expires_at = imported
        .state
        .tokens
        .expires_in
        .filter(|secs| *secs > 0)
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
    let credential = OAuthCredential {
        provider: provider.to_string(),
        access_token: imported.state.tokens.access_token.clone(),
        refresh_token: imported.state.tokens.refresh_token.clone(),
        token_type: "bearer".to_string(),
        scope: None,
        expires_at,
    };
    manager.save_credential(credential.clone()).await?;
    Ok(Some(credential))
}

fn print_auth_verify_result(result: &AuthVerifyResult) {
    println!(
        "Auth verify: provider='{}', status={}, source={}, credential_present={}, oauth_state_present={}{}{}",
        result.provider,
        result.outcome.as_str(),
        result.source,
        result.credential_present,
        result.oauth_state_present,
        result
            .expires_at
            .as_deref()
            .map(|v| format!(", expires_at={v}"))
            .unwrap_or_default(),
        result
            .detail
            .as_deref()
            .map(|v| format!(", detail={v}"))
            .unwrap_or_default()
    );
}

fn nous_auth_error_requires_fresh_login(err: &AgentError) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("invalid_grant")
        || text.contains("refresh token reuse")
        || text.contains("refresh session has been revoked")
        || text.contains("session has been revoked")
        || text.contains("stored nous auth state is invalid")
        || text.contains("missing refresh token")
        || text.contains("no refresh token")
}

async fn save_nous_runtime_credential(
    manager: &AuthManager,
    resolved: &NousRuntimeCredentials,
) -> Result<(), AgentError> {
    manager
        .save_credential(OAuthCredential {
            provider: "nous".to_string(),
            access_token: resolved.api_key.clone(),
            refresh_token: resolved.refresh_token.clone(),
            token_type: resolved.token_type.clone(),
            scope: resolved.scope.clone(),
            expires_at: parse_rfc3339_utc(resolved.expires_at.as_deref()),
        })
        .await
}

async fn fresh_nous_login_and_save(
    manager: &AuthManager,
) -> Result<(NousRuntimeCredentials, std::path::PathBuf, NousAuthState), AgentError> {
    let _ = clear_provider_auth_state("nous")?;
    let state = login_nous_device_code(NousDeviceCodeOptions::default()).await?;
    let auth_path = save_nous_auth_state(&state)?;
    let resolved = resolve_nous_runtime_credentials(
        true,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await?;
    save_nous_runtime_credential(manager, &resolved).await?;
    Ok((resolved, auth_path, state))
}

async fn resolve_or_fresh_login_nous(
    manager: &AuthManager,
    use_existing: bool,
) -> Result<
    (
        NousRuntimeCredentials,
        std::path::PathBuf,
        bool,
        NousAuthState,
    ),
    AgentError,
> {
    if use_existing {
        if let Some(imported) = discover_existing_nous_oauth()? {
            println!(
                "Detected existing Nous OAuth session at {}.",
                imported.source_path.display()
            );
            let imported_state = imported.state.clone();
            let auth_path = save_nous_auth_state(&imported.state)?;
            match resolve_nous_runtime_credentials(
                true,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(resolved) => {
                    save_nous_runtime_credential(manager, &resolved).await?;
                    return Ok((resolved, auth_path, true, imported_state));
                }
                Err(err) if nous_auth_error_requires_fresh_login(&err) => {
                    eprintln!(
                        "Existing Nous OAuth session is stale/revoked; starting a fresh login flow."
                    );
                }
                Err(err) => return Err(err),
            }
        }
    }
    let (resolved, auth_path, state) = fresh_nous_login_and_save(manager).await?;
    Ok((resolved, auth_path, false, state))
}

async fn verify_single_oauth_provider(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<AuthVerifyResult, AgentError> {
    let provider = normalize_auth_provider(provider);
    let env_present = provider_api_key_from_env(&provider).is_some();
    let auth_state_present = read_provider_auth_state(&provider)?.is_some();
    let mut stored_credential = token_store.get(&provider).await;

    if matches!(provider.as_str(), "openai" | "openai-codex") && stored_credential.is_none() {
        stored_credential = ensure_openai_oauth_credential(&provider, token_store, manager).await?;
    }

    let stored_present = stored_credential
        .as_ref()
        .map(|c| !c.access_token.trim().is_empty())
        .unwrap_or(false);
    let mut result = AuthVerifyResult {
        provider: provider.clone(),
        outcome: AuthVerifyOutcome::Missing,
        source: auth_verify_source(env_present, stored_present, auth_state_present),
        credential_present: env_present || stored_present,
        oauth_state_present: auth_state_present,
        expires_at: stored_credential
            .as_ref()
            .and_then(|c| c.expires_at.as_ref().map(|dt| dt.to_rfc3339())),
        detail: None,
    };

    match provider.as_str() {
        "nous" => match resolve_nous_runtime_credentials(
            false,
            true,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        )
        .await
        {
            Ok(creds) => {
                manager
                    .save_credential(OAuthCredential {
                        provider: "nous".to_string(),
                        access_token: creds.api_key,
                        refresh_token: creds.refresh_token,
                        token_type: creds.token_type,
                        scope: creds.scope,
                        expires_at: parse_rfc3339_utc(creds.expires_at.as_deref()),
                    })
                    .await?;
                result.outcome = if creds.source == "portal" {
                    AuthVerifyOutcome::ValidRefreshed
                } else {
                    AuthVerifyOutcome::Valid
                };
                result.source = creds.source;
                result.expires_at = creds.expires_at;
                result.credential_present = true;
                return Ok(result);
            }
            Err(err) => {
                result.outcome = if env_present || stored_present || auth_state_present {
                    AuthVerifyOutcome::RefreshFailed
                } else {
                    AuthVerifyOutcome::Missing
                };
                result.detail = Some(err.to_string());
                return Ok(result);
            }
        },
        "qwen-oauth" => match resolve_qwen_runtime_credentials(
            false,
            true,
            QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        )
        .await
        {
            Ok(creds) => {
                manager
                    .save_credential(OAuthCredential {
                        provider: "qwen-oauth".to_string(),
                        access_token: creds.api_key.clone(),
                        refresh_token: creds.refresh_token,
                        token_type: creds.token_type,
                        scope: None,
                        expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                    })
                    .await?;
                result.outcome = if creds.expires_at_ms.is_some() {
                    AuthVerifyOutcome::ValidRefreshed
                } else {
                    AuthVerifyOutcome::Valid
                };
                result.source = creds.source;
                result.expires_at = creds
                    .expires_at_ms
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|dt| dt.to_rfc3339());
                result.credential_present = true;
                return Ok(result);
            }
            Err(err) => {
                result.outcome = if env_present || stored_present || auth_state_present {
                    AuthVerifyOutcome::RefreshFailed
                } else {
                    AuthVerifyOutcome::Missing
                };
                result.detail = Some(err.to_string());
                return Ok(result);
            }
        },
        "google-gemini-cli" => match resolve_gemini_oauth_runtime_credentials(false).await {
            Ok(creds) => {
                manager
                    .save_credential(OAuthCredential {
                        provider: "google-gemini-cli".to_string(),
                        access_token: creds.api_key,
                        refresh_token: creds.refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                    })
                    .await?;
                result.outcome = AuthVerifyOutcome::Valid;
                result.source = creds.source;
                result.expires_at = creds
                    .expires_at_ms
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|dt| dt.to_rfc3339());
                result.credential_present = true;
                return Ok(result);
            }
            Err(err) => {
                result.outcome = if env_present || stored_present || auth_state_present {
                    AuthVerifyOutcome::RefreshFailed
                } else {
                    AuthVerifyOutcome::Missing
                };
                result.detail = Some(err.to_string());
                return Ok(result);
            }
        },
        "anthropic" => {
            let oauth_state = read_provider_auth_state("anthropic")?;
            let refresh_token = oauth_state.as_ref().and_then(|state| {
                let object = state.as_object()?;
                object
                    .get("refresh_token")
                    .or_else(|| object.get("refreshToken"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });
            let status = get_anthropic_oauth_status().await;
            if status.logged_in && status.api_key.is_some() {
                result.outcome = AuthVerifyOutcome::Valid;
                result.source = status
                    .source
                    .clone()
                    .unwrap_or_else(|| "anthropic_oauth".to_string());
                result.expires_at = status
                    .expires_at_ms
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|dt| dt.to_rfc3339());
                result.credential_present = true;
                return Ok(result);
            }
            if let Some(refresh_token) = refresh_token {
                match refresh_oauth_store_credential(
                    "anthropic",
                    &OAuthCredential {
                        provider: "anthropic".to_string(),
                        access_token: status.api_key.unwrap_or_default(),
                        refresh_token: Some(refresh_token.clone()),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(status.expires_at_ms),
                    },
                )
                .await
                {
                    Ok(refreshed) => {
                        manager.save_credential(refreshed.clone()).await?;
                        let expires_at_ms = refreshed.expires_at.map(|dt| dt.timestamp_millis());
                        let auth_state = serde_json::json!({
                            "access_token": refreshed.access_token,
                            "refresh_token": refreshed.refresh_token,
                            "expires_at_ms": expires_at_ms,
                            "source": "hermes_pkce_refresh",
                        });
                        let _ = save_provider_auth_state("anthropic", auth_state)?;
                        result.outcome = AuthVerifyOutcome::ValidRefreshed;
                        result.source = "hermes_pkce_refresh".to_string();
                        result.expires_at = refreshed.expires_at.map(|dt| dt.to_rfc3339());
                        result.credential_present = true;
                        return Ok(result);
                    }
                    Err(err) => {
                        result.outcome = AuthVerifyOutcome::RefreshFailed;
                        result.detail = Some(err.to_string());
                        return Ok(result);
                    }
                }
            }
            if let Some(expires_ms) = status.expires_at_ms {
                let expired = chrono::Utc::now().timestamp_millis() >= expires_ms;
                if expired {
                    result.outcome = AuthVerifyOutcome::Expired;
                    result.expires_at = chrono::DateTime::from_timestamp_millis(expires_ms)
                        .map(|dt| dt.to_rfc3339());
                } else {
                    result.outcome = AuthVerifyOutcome::Unverified;
                }
            } else {
                result.outcome = if env_present {
                    AuthVerifyOutcome::Unverified
                } else {
                    AuthVerifyOutcome::Missing
                };
            }
            if let Some(err) = status.error {
                result.detail = Some(err);
            }
            return Ok(result);
        }
        "openai" | "openai-codex" => {
            if let Some(credential) = stored_credential {
                if !credential.is_expired(60) && !credential.access_token.trim().is_empty() {
                    result.outcome = AuthVerifyOutcome::Valid;
                    result.expires_at = credential.expires_at.map(|dt| dt.to_rfc3339());
                    result.credential_present = true;
                    return Ok(result);
                }
                if credential
                    .refresh_token
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|v| !v.is_empty())
                {
                    match refresh_oauth_store_credential(provider.as_str(), &credential).await {
                        Ok(refreshed) => {
                            manager.save_credential(refreshed.clone()).await?;
                            result.outcome = AuthVerifyOutcome::ValidRefreshed;
                            result.source = "token_store_refresh".to_string();
                            result.expires_at = refreshed.expires_at.map(|dt| dt.to_rfc3339());
                            result.credential_present = true;
                            return Ok(result);
                        }
                        Err(err) => {
                            result.outcome = AuthVerifyOutcome::RefreshFailed;
                            result.detail = Some(err.to_string());
                            return Ok(result);
                        }
                    }
                }
                result.outcome = AuthVerifyOutcome::Expired;
                result.expires_at = credential.expires_at.map(|dt| dt.to_rfc3339());
                return Ok(result);
            }
            if env_present {
                result.outcome = AuthVerifyOutcome::Unverified;
                result.detail = Some(
                    "Environment token is present but no OAuth credential state was available."
                        .to_string(),
                );
                return Ok(result);
            }
            result.outcome = AuthVerifyOutcome::Missing;
            return Ok(result);
        }
        _ => {}
    }

    if env_present {
        result.outcome = AuthVerifyOutcome::Unverified;
        result.detail = Some(
            "Provider uses env credential source; live OAuth verification is unavailable.".into(),
        );
    } else if stored_present {
        if let Some(cred) = stored_credential {
            if cred.is_expired(60) {
                result.outcome = AuthVerifyOutcome::Expired;
                result.expires_at = cred.expires_at.map(|dt| dt.to_rfc3339());
            } else {
                result.outcome = AuthVerifyOutcome::Valid;
            }
        } else {
            result.outcome = AuthVerifyOutcome::Valid;
        }
    } else {
        result.outcome = AuthVerifyOutcome::Missing;
    }
    Ok(result)
}

async fn run_auth_verify(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<(), AgentError> {
    let targets: Vec<String> = if provider == "all" || provider == "*" {
        hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
            .iter()
            .map(|p| p.to_string())
            .collect()
    } else {
        vec![normalize_auth_provider(provider)]
    };

    let mut failed: Vec<AuthVerifyResult> = Vec::new();
    for target in targets {
        if !provider_supports_oauth(&target) {
            let result = AuthVerifyResult {
                provider: target.clone(),
                outcome: AuthVerifyOutcome::Unverified,
                source: "unsupported".to_string(),
                credential_present: provider_api_key_from_env(&target).is_some(),
                oauth_state_present: false,
                expires_at: None,
                detail: Some("Provider is not OAuth-capable in Hermes Ultra.".to_string()),
            };
            print_auth_verify_result(&result);
            continue;
        }
        let result = verify_single_oauth_provider(&target, token_store, manager).await?;
        print_auth_verify_result(&result);
        if !result.outcome.is_success() {
            failed.push(result);
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        let failed_ids: Vec<String> = failed.iter().map(|r| r.provider.clone()).collect();
        Err(AgentError::AuthFailed(format!(
            "OAuth verification failed for provider(s): {}",
            failed_ids.join(", ")
        )))
    }
}

async fn run_auth(
    cli: Cli,
    action: Option<String>,
    provider: Option<String>,
    target: Option<String>,
    auth_type: Option<String>,
    label: Option<String>,
    api_key: Option<String>,
    qr: bool,
) -> Result<(), AgentError> {
    let provider = resolve_auth_provider(provider);
    let auth_store_path = secret_vault_path_for_cli(&cli);
    let token_store = FileTokenStore::new(auth_store_path).await?;
    let manager = AuthManager::new(token_store.clone());
    let pool_path = auth_pool_path_for_cli(&cli);
    let mut pool_store = load_auth_pool_store(&pool_path)?;
    match action.as_deref().unwrap_or("status") {
        "add" => {
            let provider = normalize_auth_provider(provider.trim());
            let mut auth_type = resolve_auth_type_for_provider(&provider, auth_type.as_deref());

            if auth_type == "oauth" {
                match provider.as_str() {
                    "nous" => {
                        let (resolved, auth_path, _imported_existing, state) =
                            resolve_or_fresh_login_nous(&manager, true).await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: state
                                .agent_key_obtained_at
                                .as_deref()
                                .map(|_| "device_code".to_string())
                                .unwrap_or_else(|| "discovered_session".to_string()),
                            access_token: resolved.api_key,
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Nous OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "openai-codex" => {
                        let imported = discover_existing_openai_codex_oauth()?;
                        let state = if let Some(imported) = imported {
                            println!(
                                "Detected existing OpenAI Codex OAuth session at {}.",
                                imported.source_path.display()
                            );
                            imported.state
                        } else {
                            login_openai_codex_device_code(CodexDeviceCodeOptions::default())
                                .await?
                        };
                        let auth_path = save_codex_auth_state(&state)?;
                        let expires_at = state
                            .tokens
                            .expires_in
                            .filter(|secs| *secs > 0)
                            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                        manager
                            .save_credential(OAuthCredential {
                                provider: "openai-codex".to_string(),
                                access_token: state.tokens.access_token.clone(),
                                refresh_token: state.tokens.refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at,
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: state
                                .source
                                .clone()
                                .unwrap_or_else(|| "device_code".to_string()),
                            access_token: state.tokens.access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added OpenAI Codex OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "openai" => {
                        let imported = discover_existing_openai_oauth()?;
                        let state = if let Some(imported) = imported {
                            println!(
                                "Detected existing OpenAI OAuth session at {}.",
                                imported.source_path.display()
                            );
                            imported.state
                        } else {
                            login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                        };
                        let auth_path = save_openai_auth_state(&state)?;
                        let expires_at = state
                            .tokens
                            .expires_in
                            .filter(|secs| *secs > 0)
                            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                        manager
                            .save_credential(OAuthCredential {
                                provider: "openai".to_string(),
                                access_token: state.tokens.access_token.clone(),
                                refresh_token: state.tokens.refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at,
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: "device_code".to_string(),
                            access_token: state.tokens.access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added OpenAI OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "anthropic" => {
                        let imported = discover_existing_anthropic_oauth()?;
                        let (state, source_label) = if let Some(imported) = imported {
                            println!(
                                "Detected existing Anthropic OAuth session at {}.",
                                imported.source_path.display()
                            );
                            (imported.state, imported.source)
                        } else {
                            (
                                login_anthropic_oauth(AnthropicOAuthLoginOptions::default())
                                    .await?,
                                "hermes_pkce".to_string(),
                            )
                        };
                        let access_token = state.access_token.clone();
                        let refresh_token = state.refresh_token.clone();
                        let expires_at_ms = state.expires_at_ms;
                        let auth_state = serde_json::json!({
                            "access_token": access_token.clone(),
                            "refresh_token": refresh_token.clone(),
                            "expires_at_ms": expires_at_ms,
                            "source": source_label.clone(),
                        });
                        let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "anthropic".to_string(),
                                access_token: access_token.clone(),
                                refresh_token: refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: source_label,
                            access_token: access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Anthropic OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "qwen-oauth" => {
                        let creds = resolve_qwen_runtime_credentials(
                            false,
                            true,
                            QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                        )
                        .await?;
                        let auth_state = serde_json::to_value(&creds.tokens)
                            .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                        let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "qwen-oauth".to_string(),
                                access_token: creds.api_key.clone(),
                                refresh_token: creds.refresh_token.clone(),
                                token_type: creds.token_type.clone(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: creds.source.clone(),
                            access_token: creds.api_key.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Qwen OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Qwen auth file: {}", creds.auth_file.display());
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "google-gemini-cli" => {
                        let creds =
                            login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default())
                                .await?;
                        let access_token = creds.api_key.clone();
                        let refresh_token = creds.refresh_token.clone();
                        let expires_at_ms = creds.expires_at_ms;
                        let email = creds.email.clone();
                        let project_id = creds.project_id.clone();
                        let source = creds.source.clone();
                        let auth_state = serde_json::json!({
                            "access_token": access_token.clone(),
                            "refresh_token": refresh_token.clone(),
                            "expires_at_ms": expires_at_ms,
                            "email": email.clone(),
                            "project_id": project_id.clone(),
                            "source": source.clone(),
                        });
                        let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "google-gemini-cli".to_string(),
                                access_token: access_token.clone(),
                                refresh_token: refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or_else(|| email.clone().unwrap_or(default_label)),
                            auth_type: "oauth".to_string(),
                            source: source,
                            access_token: access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Google Gemini OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Google auth file: {}", creds.auth_file.display());
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    _ => {
                        println!(
                            "OAuth flow is not implemented for provider '{}'; falling back to API key/manual token login.",
                            provider
                        );
                        auth_type = "api_key".to_string();
                    }
                }
            }

            let token = if let Some(raw) = api_key {
                raw.trim().to_string()
            } else {
                resolve_llm_login_token(&cli, &provider).await?
            };
            if token.is_empty() {
                return Err(AgentError::Config("auth add: empty credential".into()));
            }
            let entries = pool_store.providers.entry(provider.clone()).or_default();
            let default_label = format!("{provider}-{}", entries.len() + 1);
            let entry = AuthPoolEntry {
                id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                label: label.unwrap_or(default_label),
                auth_type,
                source: "manual".to_string(),
                access_token: token.clone(),
                last_status: None,
                last_status_at: None,
                last_error_code: None,
            };
            entries.push(entry.clone());
            save_auth_pool_store(&pool_path, &pool_store)?;
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token: entry.access_token.clone(),
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            println!(
                "Added pooled credential for provider '{}' (label='{}', id={}).",
                provider, entry.label, entry.id
            );
            return Ok(());
        }
        "list" => {
            if pool_store.providers.is_empty() {
                println!("No pooled credentials configured.");
                return Ok(());
            }
            if let Some(entries) = pool_store.providers.get(&provider) {
                println!("{} ({} credentials):", provider, entries.len());
                for (idx, e) in entries.iter().enumerate() {
                    let exhausted = if e.last_status.as_deref() == Some("exhausted") {
                        " exhausted"
                    } else {
                        ""
                    };
                    println!(
                        "  #{}  {:<20} {:<8} {}{}",
                        idx + 1,
                        e.label,
                        e.auth_type,
                        e.source,
                        exhausted
                    );
                }
                return Ok(());
            }
            println!("No pooled credentials for provider '{}'.", provider);
            return Ok(());
        }
        "remove" => {
            let target = target.ok_or_else(|| {
                AgentError::Config(
                    "auth remove usage: hermes auth remove <provider> <index|id|label>".into(),
                )
            })?;
            let Some(entries) = pool_store.providers.get_mut(&provider) else {
                return Err(AgentError::Config(format!(
                    "No pooled credentials for provider '{}'",
                    provider
                )));
            };
            let Some(index) = resolve_pool_target(entries, &target) else {
                return Err(AgentError::Config(format!(
                    "Could not resolve auth remove target '{}' for provider '{}'",
                    target, provider
                )));
            };
            let removed = entries.remove(index);
            if entries.is_empty() {
                pool_store.providers.remove(&provider);
                token_store.remove(&provider).await?;
                if provider_supports_oauth(&provider) {
                    let _ = clear_provider_auth_state(&provider)?;
                }
            } else if let Some(next) = entries.first() {
                manager
                    .save_credential(OAuthCredential {
                        provider: provider.clone(),
                        access_token: next.access_token.clone(),
                        refresh_token: None,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: None,
                    })
                    .await?;
            }
            save_auth_pool_store(&pool_path, &pool_store)?;
            println!(
                "Removed pooled credential for provider '{}' (label='{}', id={}).",
                provider, removed.label, removed.id
            );
            return Ok(());
        }
        "reset" => {
            let Some(entries) = pool_store.providers.get_mut(&provider) else {
                println!("No pooled credentials for provider '{}'.", provider);
                return Ok(());
            };
            let mut reset = 0usize;
            for e in entries.iter_mut() {
                if e.last_status.is_some() || e.last_error_code.is_some() {
                    e.last_status = None;
                    e.last_status_at = None;
                    e.last_error_code = None;
                    reset += 1;
                }
            }
            save_auth_pool_store(&pool_path, &pool_store)?;
            println!(
                "Reset status on {} pooled credential(s) for provider '{}'.",
                reset, provider
            );
            return Ok(());
        }
        "verify" => {
            run_auth_verify(&provider, &token_store, &manager).await?;
            return Ok(());
        }
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
            if provider == "qqbot" {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_QQBOT_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);

                let existing_app_id = disk
                    .platforms
                    .get("qqbot")
                    .and_then(|p| p.extra.get("app_id"))
                    .and_then(|v| v.as_str());
                let existing_secret = disk
                    .platforms
                    .get("qqbot")
                    .and_then(|p| p.extra.get("client_secret"))
                    .and_then(|v| v.as_str());

                let (app_id, client_secret, user_openid) = if qr_preferred {
                    let portal_host = qqbot_portal_host_from_disk(&disk);
                    let (create_path, poll_path) = qqbot_onboard_endpoints_from_disk(&disk);
                    match qqbot_qr_login_flow(&portal_host, &create_path, &poll_path, 600).await {
                        Ok(tuple) => tuple,
                        Err(e) => {
                            println!(
                                "QQBot QR setup failed, falling back to manual credentials: {}",
                                e
                            );
                            let app_id = qqbot_app_id_from_env_or_prompt(existing_app_id).await?;
                            let client_secret =
                                qqbot_client_secret_from_env_or_prompt(existing_secret).await?;
                            (app_id, client_secret, String::new())
                        }
                    }
                } else {
                    let app_id = qqbot_app_id_from_env_or_prompt(existing_app_id).await?;
                    let client_secret =
                        qqbot_client_secret_from_env_or_prompt(existing_secret).await?;
                    (app_id, client_secret, String::new())
                };

                let qq = disk
                    .platforms
                    .entry("qqbot".to_string())
                    .or_insert_with(PlatformConfig::default);
                qq.enabled = true;
                qq.extra.insert(
                    "app_id".to_string(),
                    serde_json::Value::String(app_id.clone()),
                );
                qq.extra.insert(
                    "client_secret".to_string(),
                    serde_json::Value::String(client_secret.clone()),
                );
                if !qq.extra.contains_key("markdown_support") {
                    qq.extra.insert(
                        "markdown_support".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                if !user_openid.trim().is_empty() {
                    qq.extra.insert(
                        "user_openid".to_string(),
                        serde_json::Value::String(user_openid.clone()),
                    );
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "QQBot: app_id/client_secret saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "wecom" {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_WECOM_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);

                let existing_bot_id = disk
                    .platforms
                    .get("wecom")
                    .and_then(|p| p.extra.get("bot_id"))
                    .and_then(|v| v.as_str());
                let existing_secret = disk
                    .platforms
                    .get("wecom")
                    .and_then(|p| p.extra.get("secret"))
                    .and_then(|v| v.as_str());

                let (bot_id, secret) = if qr_preferred {
                    match wecom_qr_login_flow(300).await {
                        Ok(pair) => pair,
                        Err(e) => {
                            println!("WeCom QR login failed, falling back to manual input: {e}");
                            let bot_id = wecom_bot_id_from_env_or_prompt(existing_bot_id).await?;
                            let secret = wecom_secret_from_env_or_prompt(existing_secret).await?;
                            (bot_id, secret)
                        }
                    }
                } else {
                    let bot_id = wecom_bot_id_from_env_or_prompt(existing_bot_id).await?;
                    let secret = wecom_secret_from_env_or_prompt(existing_secret).await?;
                    (bot_id, secret)
                };

                let wecom = disk
                    .platforms
                    .entry("wecom".to_string())
                    .or_insert_with(PlatformConfig::default);
                wecom.enabled = true;
                wecom.extra.insert(
                    "bot_id".to_string(),
                    serde_json::Value::String(bot_id.clone()),
                );
                wecom.extra.insert(
                    "secret".to_string(),
                    serde_json::Value::String(secret.clone()),
                );
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "WeCom: bot_id/secret saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                configure_platform_basic_prompts(&mut disk, platform_key).await?;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "{}: config updated and platform enabled in {}",
                    platform_key,
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "nous" {
                let (_resolved, auth_path, _imported_existing, _state) =
                    resolve_or_fresh_login_nous(&manager, true).await?;
                println!("Nous OAuth credential saved as provider 'nous'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "openai-codex" {
                let imported = discover_existing_openai_codex_oauth()?;
                let state = if let Some(imported) = imported {
                    println!(
                        "Detected existing OpenAI Codex OAuth session at {}.",
                        imported.source_path.display()
                    );
                    imported.state
                } else {
                    login_openai_codex_device_code(CodexDeviceCodeOptions::default()).await?
                };
                let auth_path = save_codex_auth_state(&state)?;
                let expires_at = state
                    .tokens
                    .expires_in
                    .filter(|secs| *secs > 0)
                    .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                manager
                    .save_credential(OAuthCredential {
                        provider: "openai-codex".to_string(),
                        access_token: state.tokens.access_token.clone(),
                        refresh_token: state.tokens.refresh_token.clone(),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at,
                    })
                    .await?;
                println!("OpenAI Codex OAuth credential saved as provider 'openai-codex'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "openai" {
                let imported = discover_existing_openai_oauth()?;
                let state = if let Some(imported) = imported {
                    println!(
                        "Detected existing OpenAI OAuth session at {}.",
                        imported.source_path.display()
                    );
                    imported.state
                } else {
                    login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                };
                let auth_path = save_openai_auth_state(&state)?;
                let expires_at = state
                    .tokens
                    .expires_in
                    .filter(|secs| *secs > 0)
                    .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                manager
                    .save_credential(OAuthCredential {
                        provider: "openai".to_string(),
                        access_token: state.tokens.access_token.clone(),
                        refresh_token: state.tokens.refresh_token.clone(),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at,
                    })
                    .await?;
                println!("OpenAI OAuth login complete; credential saved as provider 'openai'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "anthropic" {
                let imported = discover_existing_anthropic_oauth()?;
                let (state, source_label) = if let Some(imported) = imported {
                    println!(
                        "Detected existing Anthropic OAuth session at {}.",
                        imported.source_path.display()
                    );
                    (imported.state, imported.source)
                } else {
                    (
                        login_anthropic_oauth(AnthropicOAuthLoginOptions::default()).await?,
                        "hermes_pkce".to_string(),
                    )
                };
                let access_token = state.access_token.clone();
                let refresh_token = state.refresh_token.clone();
                let expires_at_ms = state.expires_at_ms;
                let auth_state = serde_json::json!({
                    "access_token": access_token.clone(),
                    "refresh_token": refresh_token.clone(),
                    "expires_at_ms": expires_at_ms,
                    "source": source_label,
                });
                let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "anthropic".to_string(),
                        access_token,
                        refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(expires_at_ms),
                    })
                    .await?;
                println!("Anthropic OAuth credential saved as provider 'anthropic'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "qwen-oauth" {
                let creds = resolve_qwen_runtime_credentials(
                    false,
                    true,
                    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                )
                .await?;
                let auth_state = serde_json::to_value(&creds.tokens)
                    .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "qwen-oauth".to_string(),
                        access_token: creds.api_key.clone(),
                        refresh_token: creds.refresh_token.clone(),
                        token_type: creds.token_type.clone(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                    })
                    .await?;
                println!(
                    "Qwen OAuth credential imported from {} and stored as provider 'qwen-oauth'.",
                    creds.auth_file.display()
                );
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "google-gemini-cli" {
                let creds =
                    login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default()).await?;
                let access_token = creds.api_key.clone();
                let refresh_token = creds.refresh_token.clone();
                let expires_at_ms = creds.expires_at_ms;
                let auth_state = serde_json::json!({
                    "access_token": access_token.clone(),
                    "refresh_token": refresh_token.clone(),
                    "expires_at_ms": expires_at_ms,
                    "email": creds.email.clone(),
                    "project_id": creds.project_id.clone(),
                    "source": creds.source.clone(),
                });
                let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "google-gemini-cli".to_string(),
                        access_token,
                        refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(expires_at_ms),
                    })
                    .await?;
                println!(
                    "Google Gemini OAuth login complete; credential saved as provider 'google-gemini-cli'."
                );
                println!("Google auth file: {}", creds.auth_file.display());
                println!("Saved OAuth state: {}", auth_path.display());
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
                println!(
                    "Ensure GITHUB_COPILOT_TOKEN is set for the agent (see printed instructions above)."
                );
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
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(p) = disk.platforms.get_mut(platform_key) {
                    p.enabled = false;
                    p.token = None;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "{}: disabled and token cleared in {}",
                    platform_key,
                    cfg_path.display()
                );
                return Ok(());
            }
            let msg = hermes_cli::auth::logout(&provider).await?;
            token_store.remove(&provider).await?;
            if provider_supports_oauth(&provider) {
                let _ = clear_provider_auth_state(&provider)?;
            }
            println!("{} (removed credential for provider: {})", msg, provider);
        }
        _ => {
            if provider == "all" || provider == "*" {
                print_auth_status_matrix(&cli, &manager).await?;
                return Ok(());
            }
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
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (enabled, token_present) = disk
                    .platforms
                    .get(platform_key)
                    .map(|p| (p.enabled, platform_token_or_extra(p).is_some()))
                    .unwrap_or((false, false));
                println!(
                    "{} ({}): credential_present={} enabled={}",
                    platform_key,
                    cfg_path.display(),
                    token_present,
                    enabled
                );
                return Ok(());
            }
            if provider == "qwen-oauth" {
                let qwen_status = get_qwen_auth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = manager.get_access_token(&provider).await?.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Qwen OAuth: logged_in={} auth_file={} source={} expires_at_ms={}",
                    qwen_status.logged_in,
                    qwen_status.auth_file.display(),
                    qwen_status.source.as_deref().unwrap_or("none"),
                    qwen_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(token) = qwen_status.api_key.as_deref() {
                    println!("Qwen OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = qwen_status.error.as_deref() {
                    println!("Qwen OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            if provider == "google-gemini-cli" {
                let google_status = get_gemini_oauth_auth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = manager.get_access_token(&provider).await?.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Google Gemini OAuth: logged_in={} auth_file={} source={} expires_at_ms={}",
                    google_status.logged_in,
                    google_status.auth_file.display(),
                    google_status.source.as_deref().unwrap_or("none"),
                    google_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(email) = google_status.email.as_deref() {
                    println!("Google account: {}", email);
                }
                if let Some(project_id) = google_status.project_id.as_deref() {
                    println!("Google project_id: {}", project_id);
                }
                if let Some(token) = google_status.api_key.as_deref() {
                    println!("Google OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = google_status.error.as_deref() {
                    println!("Google OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            if provider == "anthropic" {
                let anthropic_status = get_anthropic_oauth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = manager.get_access_token(&provider).await?.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Anthropic OAuth: logged_in={} source={} expires_at_ms={}",
                    anthropic_status.logged_in,
                    anthropic_status.source.as_deref().unwrap_or("none"),
                    anthropic_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(token) = anthropic_status.api_key.as_deref() {
                    println!("Anthropic OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = anthropic_status.error.as_deref() {
                    println!("Anthropic OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            let env_present = provider_api_key_from_env(&provider).is_some();
            let store_present = manager.get_access_token(&provider).await?.is_some();
            let auth_state_present = if provider_supports_oauth(&provider) {
                read_provider_auth_state(&provider)?.is_some()
            } else {
                false
            };
            let (has_token, source) = if env_present {
                (true, "env")
            } else if store_present {
                (true, "token_store")
            } else if auth_state_present {
                (true, "auth_json")
            } else {
                (false, "none")
            };
            println!(
                "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                provider, has_token, source, auth_state_present
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
            println!(
                "Saved secret for provider '{provider}' in {}",
                path.display()
            );
            if let Some(env_var) = provider_env_var(&provider) {
                println!("Mapped runtime env: {env_var}");
            }
        }
        "get" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets get: usage `hermes secrets get <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            if let Some((stored_provider, secret)) =
                lookup_secret_from_vault(&store, &provider).await
            {
                if show {
                    if !secret_stdout_allowed() {
                        return Err(AgentError::Config(
                            "Refusing plaintext secret output. Re-run with HERMES_ALLOW_SECRET_STDOUT=1 to opt in."
                                .into(),
                        ));
                    }
                    println!("{secret}");
                } else {
                    println!("{}", mask_secret(&secret));
                }
                if stored_provider != provider {
                    println!("(resolved via provider alias '{}')", stored_provider);
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
                AgentError::Config(
                    "secrets remove: usage `hermes secrets remove <provider>`".into(),
                )
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
    let terminal_backend = build_terminal_backend(&config);
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

fn parse_deliver_config(raw: &str) -> Option<hermes_cron::DeliverConfig> {
    let trimmed = raw.trim();
    let (head, chat_id) = trimmed
        .split_once(':')
        .map(|(p, rest)| (p, Some(rest.to_string())))
        .unwrap_or((trimmed, None));
    let value = head.trim().to_ascii_lowercase();
    let target = match value.as_str() {
        "origin" => hermes_cron::DeliverTarget::Origin,
        "local" => hermes_cron::DeliverTarget::Local,
        "telegram" => hermes_cron::DeliverTarget::Telegram,
        "discord" => hermes_cron::DeliverTarget::Discord,
        "slack" => hermes_cron::DeliverTarget::Slack,
        "email" => hermes_cron::DeliverTarget::Email,
        "whatsapp" => hermes_cron::DeliverTarget::WhatsApp,
        "signal" => hermes_cron::DeliverTarget::Signal,
        "matrix" => hermes_cron::DeliverTarget::Matrix,
        "mattermost" => hermes_cron::DeliverTarget::Mattermost,
        "dingtalk" => hermes_cron::DeliverTarget::DingTalk,
        "feishu" => hermes_cron::DeliverTarget::Feishu,
        "wecom" => hermes_cron::DeliverTarget::WeCom,
        "weixin" | "wechat" | "wx" => hermes_cron::DeliverTarget::Weixin,
        "bluebubbles" | "imessage" => hermes_cron::DeliverTarget::BlueBubbles,
        "sms" => hermes_cron::DeliverTarget::Sms,
        "homeassistant" | "ha" => hermes_cron::DeliverTarget::HomeAssistant,
        "ntfy" => hermes_cron::DeliverTarget::Ntfy,
        _ => return None,
    };
    let platform = chat_id.map(|s| s.split(':').next().unwrap_or(s.as_str()).trim().to_string());
    Some(hermes_cron::DeliverConfig { target, platform })
}

#[allow(clippy::too_many_arguments)]
async fn run_cron(
    cli: Cli,
    action: Option<String>,
    job_id: Option<String>,
    id: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
    name: Option<String>,
    deliver: Option<String>,
    repeat: Option<u32>,
    skills: Vec<String>,
    add_skills: Vec<String>,
    remove_skills: Vec<String>,
    clear_skills: bool,
    script: Option<String>,
    no_agent: bool,
    agent: bool,
    script_timeout_seconds: Option<u64>,
    script_shell: Option<String>,
    all: bool,
) -> Result<(), AgentError> {
    let data_dir = hermes_state_root(&cli).join("cron");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| AgentError::Io(format!("cron dir {}: {}", data_dir.display(), e)))?;
    let sched = cron_scheduler_for_data_dir(data_dir.clone());
    sched.load_persisted_jobs().await.map_err(cron_cli_error)?;
    let resolved_id = job_id.or(id).filter(|s| !s.trim().is_empty());

    match action.as_deref().unwrap_or("list") {
        "list" => {
            let mut jobs = sched.list_jobs().await;
            jobs.sort_by(|a, b| a.id.cmp(&b.id));
            if jobs.is_empty() {
                println!("(no cron jobs in {})", data_dir.display());
                return Ok(());
            }
            println!("Cron jobs ({}):", data_dir.display());
            for j in jobs {
                if !all && matches!(j.status, hermes_cron::JobStatus::Completed) {
                    continue;
                }
                let snippet: String = j.prompt.chars().take(48).collect();
                println!(
                    "  {}  [{}]  {:?}  next_run={:?}  {}",
                    j.id, j.schedule, j.status, j.next_run, snippet
                );
            }
        }
        "create" | "add" => {
            let schedule = schedule.unwrap_or_else(|| "0 * * * *".to_string());
            let prompt = match prompt {
                Some(p) => p,
                None if no_agent => "[script-only cron job]".to_string(),
                None => {
                    return Err(AgentError::Config(
                        "cron create: use --prompt \"...\" (or pass --no-agent with --script)"
                            .into(),
                    ));
                }
            };
            let mut job = hermes_cron::CronJob::new(schedule, prompt);
            if let Some(name) = name.filter(|s| !s.trim().is_empty()) {
                job.name = Some(name);
            }
            if let Some(raw) = deliver.as_deref() {
                if let Some(cfg) = parse_deliver_config(raw) {
                    job.deliver = Some(cfg);
                } else {
                    return Err(AgentError::Config(format!(
                        "Unknown deliver target '{}'",
                        raw
                    )));
                }
            }
            if let Some(repeat) = repeat {
                job.repeat = Some(repeat);
            }
            if !skills.is_empty() {
                job.skills = Some(skills.clone());
            }
            if let Some(script) = script {
                if !script.trim().is_empty() {
                    job.script = Some(script);
                }
            }
            if no_agent {
                job.no_agent = true;
            }
            if agent {
                job.no_agent = false;
            }
            if job.no_agent && job.script.as_ref().map_or(true, |s| s.trim().is_empty()) {
                return Err(AgentError::Config(
                    "cron create: --no-agent requires --script".into(),
                ));
            }
            if let Some(timeout_secs) = script_timeout_seconds.filter(|v| *v > 0) {
                job.script_timeout_seconds = Some(timeout_secs);
            }
            if let Some(shell) = script_shell.filter(|v| !v.trim().is_empty()) {
                job.script_shell = Some(shell.trim().to_string());
            }
            let jid = sched.create_job(job).await.map_err(cron_cli_error)?;
            println!(
                "Created cron job id={} (persisted under {})",
                jid,
                data_dir.display()
            );
        }
        "edit" => {
            let jid = resolved_id
                .ok_or_else(|| AgentError::Config("cron edit: use <job-id> or --id".into()))?;
            let mut job = sched
                .get_job(&jid)
                .await
                .ok_or_else(|| AgentError::Config(format!("unknown job id: {}", jid)))?;

            if let Some(schedule) = schedule {
                job.schedule = schedule;
                job.schedule_spec = None;
                job.schedule_display = None;
                job.next_run = None;
                job.normalize_schedule();
                job.refresh_next_run();
            }
            if let Some(prompt) = prompt {
                job.prompt = prompt;
            }
            if let Some(name) = name {
                job.name = if name.trim().is_empty() {
                    None
                } else {
                    Some(name)
                };
            }
            if let Some(raw) = deliver.as_deref() {
                if let Some(cfg) = parse_deliver_config(raw) {
                    job.deliver = Some(cfg);
                } else {
                    return Err(AgentError::Config(format!(
                        "Unknown deliver target '{}'",
                        raw
                    )));
                }
            }
            if let Some(repeat) = repeat {
                job.repeat = Some(repeat);
            }
            if !skills.is_empty() {
                job.skills = Some(skills.clone());
            }
            if clear_skills {
                job.skills = None;
            }
            if !add_skills.is_empty() {
                let mut current = job.skills.take().unwrap_or_default();
                for skill in add_skills {
                    if !current.iter().any(|s| s == &skill) {
                        current.push(skill);
                    }
                }
                job.skills = Some(current);
            }
            if !remove_skills.is_empty() {
                let mut current = job.skills.take().unwrap_or_default();
                current.retain(|s| !remove_skills.iter().any(|r| r == s));
                job.skills = if current.is_empty() {
                    None
                } else {
                    Some(current)
                };
            }
            if let Some(script) = script {
                if script.trim().is_empty() {
                    job.script = None;
                } else {
                    job.script = Some(script);
                }
            }
            if no_agent {
                job.no_agent = true;
            }
            if agent {
                job.no_agent = false;
            }
            if let Some(timeout_secs) = script_timeout_seconds {
                job.script_timeout_seconds = if timeout_secs == 0 {
                    None
                } else {
                    Some(timeout_secs)
                };
            }
            if let Some(shell) = script_shell {
                if shell.trim().is_empty() {
                    job.script_shell = None;
                } else {
                    job.script_shell = Some(shell.trim().to_string());
                }
            }
            if job.no_agent && job.script.as_ref().map_or(true, |s| s.trim().is_empty()) {
                return Err(AgentError::Config(
                    "cron edit: no_agent mode requires a non-empty script".into(),
                ));
            }
            sched.update_job(&jid, job).await.map_err(cron_cli_error)?;
            println!("Updated job {}", jid);
        }
        "delete" | "remove" | "pause" | "resume" | "run" | "history" => {
            let act = action.as_deref().unwrap_or("cron");
            let jid = resolved_id
                .ok_or_else(|| AgentError::Config(format!("{}: use <job-id> or --id", act)))?;
            match act {
                "delete" | "remove" => {
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
                    live_sched
                        .load_persisted_jobs()
                        .await
                        .map_err(cron_cli_error)?;
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
        "status" => {
            let jobs = sched.list_jobs().await;
            let active = jobs
                .iter()
                .filter(|j| matches!(j.status, hermes_cron::JobStatus::Active))
                .count();
            println!(
                "Cron scheduler status: jobs_total={} active={} data_dir={}",
                jobs.len(),
                active,
                data_dir.display()
            );
        }
        "tick" => {
            let now = chrono::Utc::now();
            let due: Vec<String> = sched
                .list_jobs()
                .await
                .into_iter()
                .filter(|j| j.is_due(now))
                .map(|j| j.id)
                .collect();
            if due.is_empty() {
                println!("No due jobs at {}.", now);
                return Ok(());
            }
            let live_sched = build_live_cron_scheduler(&cli, &data_dir)?;
            live_sched
                .load_persisted_jobs()
                .await
                .map_err(cron_cli_error)?;
            for jid in &due {
                let result = live_sched.run_job(jid).await;
                match result {
                    Ok(_) => println!("tick: ran {}", jid),
                    Err(e) => println!("tick: {} failed ({})", jid, e),
                }
            }
            println!("Tick complete: {} job(s) processed.", due.len());
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown cron action: {} (use list|create|edit|pause|resume|run|remove|delete|history|status|tick)",
                other
            )));
        }
    }
    Ok(())
}

fn webhook_store_path(cli: &Cli) -> PathBuf {
    hermes_state_root(&cli).join("webhooks.json")
}

fn webhook_subscriptions_path(cli: &Cli) -> PathBuf {
    hermes_state_root(&cli).join("webhook_subscriptions.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WebhookSubscription {
    #[serde(default)]
    description: String,
    #[serde(default)]
    events: Vec<String>,
    secret: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default = "default_webhook_deliver")]
    deliver: String,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deliver_extra: Option<serde_json::Value>,
    #[serde(default)]
    deliver_only: bool,
}

fn default_webhook_deliver() -> String {
    "log".to_string()
}

fn load_webhook_subscriptions(
    path: &Path,
) -> Result<std::collections::BTreeMap<String, WebhookSubscription>, AgentError> {
    if !path.exists() {
        return Ok(std::collections::BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_webhook_subscriptions(
    path: &Path,
    subs: &std::collections::BTreeMap<String, WebhookSubscription>,
) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw = serde_json::to_string_pretty(subs).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
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
    name: Option<String>,
    url: Option<String>,
    id: Option<String>,
    prompt: Option<String>,
    events: Option<String>,
    description: Option<String>,
    skills: Option<String>,
    deliver: Option<String>,
    deliver_chat_id: Option<String>,
    secret: Option<String>,
    deliver_only: bool,
    payload: Option<String>,
) -> Result<(), AgentError> {
    let path = webhook_store_path(&cli);
    let mut store = hermes_cli::webhook_delivery::load_webhook_store(&path)?;
    let subs_path = webhook_subscriptions_path(&cli);
    let mut subs = load_webhook_subscriptions(&subs_path)?;

    match action.as_deref().unwrap_or("list") {
        "list" | "ls" => {
            if !subs.is_empty() {
                println!("Webhook subscriptions ({}):", subs_path.display());
                for (route, cfg) in &subs {
                    let events = if cfg.events.is_empty() {
                        "(all)".to_string()
                    } else {
                        cfg.events.join(", ")
                    };
                    println!(
                        "  {}  deliver={}  events={}  created_at={}",
                        route, cfg.deliver, events, cfg.created_at
                    );
                }
                println!();
            }
            if store.webhooks.is_empty() {
                println!("(no webhooks in {})", path.display());
                return Ok(());
            }
            println!("Webhooks ({}):", path.display());
            for w in &store.webhooks {
                println!("  {}  {}  {}", w.id, w.url, w.created_at);
            }
        }
        "subscribe" => {
            let route = name
                .ok_or_else(|| AgentError::Config("webhook subscribe: missing route name".into()))?
                .trim()
                .to_ascii_lowercase()
                .replace(' ', "-");
            if route.is_empty() {
                return Err(AgentError::Config(
                    "webhook subscribe: route name cannot be empty".into(),
                ));
            }
            let secret = secret.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let events_vec = events
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            let skills_vec = skills
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            let deliver = deliver.unwrap_or_else(|| "log".to_string());
            if deliver_only && deliver == "log" {
                return Err(AgentError::Config(
                    "--deliver-only requires --deliver to be a real target (not log)".into(),
                ));
            }
            let mut deliver_extra = None;
            if let Some(chat_id) = deliver_chat_id.filter(|s| !s.trim().is_empty()) {
                deliver_extra = Some(serde_json::json!({ "chat_id": chat_id }));
            }
            let sub = WebhookSubscription {
                description: description
                    .unwrap_or_else(|| format!("Agent-created subscription: {route}")),
                events: events_vec,
                secret: secret.clone(),
                prompt: prompt.unwrap_or_default(),
                skills: skills_vec,
                deliver: deliver.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                deliver_extra,
                deliver_only,
            };
            subs.insert(route.clone(), sub);
            save_webhook_subscriptions(&subs_path, &subs)?;
            println!("Created webhook subscription: {}", route);
            println!("  URL path: /webhooks/{}", route);
            if secret_stdout_allowed() {
                println!("  Secret: {}", secret);
                println!("  (plaintext output enabled via HERMES_ALLOW_SECRET_STDOUT=1)");
            } else {
                println!("  Secret: {}", mask_secret(&secret));
                println!("  (set HERMES_ALLOW_SECRET_STDOUT=1 to reveal plaintext once)");
            }
            println!("  Deliver: {}", deliver);
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
        "remove" | "rm" => {
            if let Some(route) = name.filter(|s| !s.trim().is_empty()) {
                if subs.remove(&route).is_some() {
                    save_webhook_subscriptions(&subs_path, &subs)?;
                    println!("Removed subscription '{}'.", route);
                    return Ok(());
                }
            }
            let before = store.webhooks.len();
            if let Some(rid) = id.filter(|s| !s.is_empty()) {
                store.webhooks.retain(|w| w.id != rid);
            } else if let Some(u) = url.filter(|s| !s.is_empty()) {
                store.webhooks.retain(|w| w.url != u);
            } else {
                return Err(AgentError::Config(
                    "webhook remove: use <name>, --id <id>, or --url <exact-url>".into(),
                ));
            }
            if store.webhooks.len() == before {
                println!("No matching webhook removed.");
            } else {
                hermes_cli::webhook_delivery::save_webhook_store(&path, &store)?;
                println!("Updated {}", path.display());
            }
        }
        "test" => {
            let route = name.ok_or_else(|| {
                AgentError::Config("webhook test: usage `hermes webhook test <name>`".into())
            })?;
            let sub = subs
                .get(&route)
                .ok_or_else(|| AgentError::Config(format!("No subscription named '{}'.", route)))?;
            let body = payload.unwrap_or_else(|| {
                r#"{"test":true,"event_type":"test","message":"Hello from hermes webhook test"}"#
                    .to_string()
            });
            let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(sub.secret.as_bytes())
                .map_err(|e| AgentError::Config(format!("webhook hmac key: {e}")))?;
            use hmac::Mac;
            mac.update(body.as_bytes());
            let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
            let cfg = load_config(cli.config_dir.as_deref())
                .map_err(|e| AgentError::Config(e.to_string()))?;
            let webhook_cfg = cfg.platforms.get("webhook");
            let host = webhook_cfg
                .and_then(|p| p.extra.get("host"))
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1");
            let port = webhook_cfg
                .and_then(|p| p.extra.get("port"))
                .and_then(|v| v.as_u64())
                .unwrap_or(8644);
            let display_host = if host == "0.0.0.0" { "127.0.0.1" } else { host };
            let target_url = format!("http://{}:{}/webhooks/{}", display_host, port, route);
            let client = reqwest::Client::new();
            let resp = client
                .post(&target_url)
                .header("Content-Type", "application/json")
                .header("X-Hub-Signature-256", sig)
                .header("X-GitHub-Event", "test")
                .body(body)
                .send()
                .await
                .map_err(|e| AgentError::Io(format!("webhook test send: {}", e)))?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            println!("Test POST {} -> {}", target_url, status);
            if !text.trim().is_empty() {
                println!("{}", text);
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown webhook action: {} (use subscribe|add|list|remove|test)",
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
    let mut cmd = hermes_cli::completion_command();
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
                println!("  Token: {}", mask_secret(&t.token));
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

fn normalize_env_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn read_env_text(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_env_key(path: &Path, key: &str) -> Option<String> {
    let raw = read_env_text(path).ok()?;
    for line in raw.lines() {
        if let Some((k, v)) = parse_env_assignment(line) {
            if k == key {
                let value = normalize_env_value(&v);
                if !value.is_empty() {
                    return Some(value);
                }
                return None;
            }
        }
    }
    None
}

const SETUP_OPENAI_ENV_KEYS: &[&str] = &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"];
const SETUP_OPENAI_CODEX_ENV_KEYS: &[&str] = &["HERMES_OPENAI_CODEX_API_KEY"];
const SETUP_ANTHROPIC_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];
const SETUP_OPENROUTER_ENV_KEYS: &[&str] = &["OPENROUTER_API_KEY"];
const SETUP_GOOGLE_GEMINI_CLI_ENV_KEYS: &[&str] = &["HERMES_GEMINI_OAUTH_API_KEY"];
const SETUP_GEMINI_ENV_KEYS: &[&str] = &["GOOGLE_API_KEY", "GEMINI_API_KEY"];
const SETUP_NOUS_ENV_KEYS: &[&str] = &["NOUS_API_KEY"];
const SETUP_QWEN_ENV_KEYS: &[&str] = &["DASHSCOPE_API_KEY"];
const SETUP_QWEN_OAUTH_ENV_KEYS: &[&str] = &["HERMES_QWEN_OAUTH_API_KEY", "DASHSCOPE_API_KEY"];
const SETUP_ALIBABA_CODING_PLAN_ENV_KEYS: &[&str] = &["ALIBABA_CODING_PLAN_API_KEY"];
const SETUP_KIMI_CODING_ENV_KEYS: &[&str] = &["KIMI_API_KEY", "KIMI_CODING_API_KEY"];
const SETUP_KIMI_CODING_CN_ENV_KEYS: &[&str] = &["KIMI_CN_API_KEY"];
const SETUP_MINIMAX_ENV_KEYS: &[&str] = &["MINIMAX_API_KEY"];
const SETUP_MINIMAX_CN_ENV_KEYS: &[&str] = &["MINIMAX_CN_API_KEY"];
const SETUP_STEPFUN_ENV_KEYS: &[&str] = &["HERMES_STEPFUN_API_KEY", "STEPFUN_API_KEY"];
const SETUP_COPILOT_ENV_KEYS: &[&str] = &["GITHUB_COPILOT_TOKEN"];
const SETUP_AI_GATEWAY_ENV_KEYS: &[&str] = &["AI_GATEWAY_API_KEY"];
const SETUP_ARCEE_ENV_KEYS: &[&str] = &["ARCEEAI_API_KEY", "ARCEE_API_KEY"];
const SETUP_DEEPSEEK_ENV_KEYS: &[&str] = &["DEEPSEEK_API_KEY"];
const SETUP_HUGGINGFACE_ENV_KEYS: &[&str] = &["HF_TOKEN"];
const SETUP_KILOCODE_ENV_KEYS: &[&str] = &["KILOCODE_API_KEY"];
const SETUP_NVIDIA_ENV_KEYS: &[&str] = &["NVIDIA_API_KEY"];
const SETUP_OLLAMA_CLOUD_ENV_KEYS: &[&str] = &["OLLAMA_API_KEY"];
const SETUP_OLLAMA_LOCAL_ENV_KEYS: &[&str] = &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"];
const SETUP_LLAMA_CPP_ENV_KEYS: &[&str] = &["LLAMA_CPP_API_KEY"];
const SETUP_VLLM_ENV_KEYS: &[&str] = &["VLLM_API_KEY"];
const SETUP_MLX_ENV_KEYS: &[&str] = &["MLX_API_KEY"];
const SETUP_APPLE_ANE_ENV_KEYS: &[&str] = &["APPLE_ANE_API_KEY"];
const SETUP_SGLANG_ENV_KEYS: &[&str] = &["SGLANG_API_KEY"];
const SETUP_TGI_ENV_KEYS: &[&str] = &["TGI_API_KEY"];
const SETUP_OPENCODE_GO_ENV_KEYS: &[&str] = &["OPENCODE_GO_API_KEY"];
const SETUP_OPENCODE_ZEN_ENV_KEYS: &[&str] = &["OPENCODE_ZEN_API_KEY"];
const SETUP_XAI_ENV_KEYS: &[&str] = &["XAI_API_KEY"];
const SETUP_XIAOMI_ENV_KEYS: &[&str] = &["XIAOMI_API_KEY"];
const SETUP_ZAI_ENV_KEYS: &[&str] = &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"];
const SETUP_BEDROCK_ENV_KEYS: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];
const HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY: &str = "HERMES_ENABLE_NOUS_MANAGED_TOOLS";

#[derive(Clone, Copy)]
struct SetupModelOption {
    provider: &'static str,
    model: &'static str,
    label: &'static str,
}

const SETUP_MODEL_OPTIONS: &[SetupModelOption] = &[
    SetupModelOption {
        provider: "nous",
        model: "nous:openai/gpt-5.5-pro",
        label: "Nous (recommended, OAuth)",
    },
    SetupModelOption {
        provider: "openai",
        model: "openai:gpt-4o",
        label: "OpenAI gpt-4o",
    },
    SetupModelOption {
        provider: "openai",
        model: "openai:gpt-4o-mini",
        label: "OpenAI gpt-4o-mini (fast & cheap)",
    },
    SetupModelOption {
        provider: "anthropic",
        model: "anthropic:claude-3-5-sonnet",
        label: "Anthropic Claude (OAuth/API key)",
    },
    SetupModelOption {
        provider: "openrouter",
        model: "openrouter:auto",
        label: "OpenRouter auto (multi-provider)",
    },
    SetupModelOption {
        provider: "openai-codex",
        model: "openai-codex:gpt-5.3-codex",
        label: "OpenAI Codex (OAuth)",
    },
    SetupModelOption {
        provider: "google-gemini-cli",
        model: "google-gemini-cli:gemini-3.1-pro-preview",
        label: "Google Gemini CLI (OAuth)",
    },
    SetupModelOption {
        provider: "gemini",
        model: "gemini:gemini-3.1-pro-preview",
        label: "Google AI Studio Gemini (API key)",
    },
    SetupModelOption {
        provider: "qwen-oauth",
        model: "qwen-oauth:qwen-plus-latest",
        label: "Qwen OAuth (CLI token)",
    },
    SetupModelOption {
        provider: "qwen",
        model: "qwen:qwen-plus-latest",
        label: "Alibaba DashScope Qwen",
    },
    SetupModelOption {
        provider: "alibaba",
        model: "alibaba:qwen-plus-latest",
        label: "Alibaba Cloud DashScope",
    },
    SetupModelOption {
        provider: "alibaba-coding-plan",
        model: "alibaba-coding-plan:qwen-plus-latest",
        label: "Alibaba Coding Plan",
    },
    SetupModelOption {
        provider: "deepseek",
        model: "deepseek:deepseek-chat",
        label: "DeepSeek",
    },
    SetupModelOption {
        provider: "kimi-coding",
        model: "kimi-coding:kimi-k2.6",
        label: "Kimi Coding (Moonshot)",
    },
    SetupModelOption {
        provider: "kimi-coding-cn",
        model: "kimi-coding-cn:kimi-k2.6",
        label: "Kimi Coding China",
    },
    SetupModelOption {
        provider: "stepfun",
        model: "stepfun:step-3.5-flash",
        label: "StepFun Step Plan",
    },
    SetupModelOption {
        provider: "minimax",
        model: "minimax:MiniMax-M2.7",
        label: "MiniMax",
    },
    SetupModelOption {
        provider: "minimax-cn",
        model: "minimax-cn:MiniMax-M2.7",
        label: "MiniMax China",
    },
    SetupModelOption {
        provider: "zai",
        model: "zai:glm-5.1",
        label: "Z.AI / GLM",
    },
    SetupModelOption {
        provider: "xai",
        model: "xai:grok-3-mini",
        label: "xAI",
    },
    SetupModelOption {
        provider: "nvidia",
        model: "nvidia:nvidia/nemotron-3-super-120b-a12b",
        label: "NVIDIA NIM",
    },
    SetupModelOption {
        provider: "huggingface",
        model: "huggingface:Qwen/Qwen3.5-397B-A17B",
        label: "Hugging Face Router",
    },
    SetupModelOption {
        provider: "opencode-go",
        model: "opencode-go:kimi-k2.6",
        label: "OpenCode Go",
    },
    SetupModelOption {
        provider: "opencode-zen",
        model: "opencode-zen:gpt-5.4",
        label: "OpenCode Zen",
    },
    SetupModelOption {
        provider: "kilocode",
        model: "kilocode:openai/gpt-5.4",
        label: "KiloCode",
    },
    SetupModelOption {
        provider: "ai-gateway",
        model: "ai-gateway:openai/gpt-5.4",
        label: "Vercel AI Gateway",
    },
    SetupModelOption {
        provider: "arcee",
        model: "arcee:trinity-large-preview",
        label: "Arcee AI",
    },
    SetupModelOption {
        provider: "xiaomi",
        model: "xiaomi:mimo-v2.5-pro",
        label: "Xiaomi MiMo",
    },
    SetupModelOption {
        provider: "ollama-cloud",
        model: "ollama-cloud:llama3.1:8b",
        label: "Ollama Cloud",
    },
    SetupModelOption {
        provider: "ollama-local",
        model: "ollama-local:qwen3:14b",
        label: "Ollama Local (OpenAI-compatible)",
    },
    SetupModelOption {
        provider: "llama-cpp",
        model: "llama-cpp:local-gguf",
        label: "llama.cpp server (local)",
    },
    SetupModelOption {
        provider: "vllm",
        model: "vllm:NousResearch/Meta-Llama-3-8B-Instruct",
        label: "vLLM server (local/self-host)",
    },
    SetupModelOption {
        provider: "mlx",
        model: "mlx:mlx-community/Qwen3-8B-4bit",
        label: "MLX server (Apple Silicon)",
    },
    SetupModelOption {
        provider: "apple-ane",
        model: "apple-ane:ane-default",
        label: "Apple ANE private endpoint",
    },
    SetupModelOption {
        provider: "sglang",
        model: "sglang:default",
        label: "SGLang OpenAI-compatible",
    },
    SetupModelOption {
        provider: "tgi",
        model: "tgi:default",
        label: "Text Generation Inference",
    },
    SetupModelOption {
        provider: "copilot",
        model: "copilot:gpt-5.4",
        label: "GitHub Copilot",
    },
];

fn default_setup_model_choice() -> usize {
    SETUP_MODEL_OPTIONS
        .iter()
        .position(|option| option.provider == "nous")
        .map(|idx| idx + 1)
        .unwrap_or(1)
}

fn setup_provider_defaults() -> Vec<SetupModelOption> {
    let mut seen = std::collections::BTreeSet::new();
    let mut providers = Vec::new();
    for option in SETUP_MODEL_OPTIONS {
        if seen.insert(option.provider) {
            providers.push(*option);
        }
    }
    providers
}

fn setup_default_model_pick_index(
    selected_provider: &str,
    current_provider_model: &str,
    displayed_suggested_models: &[String],
) -> usize {
    if displayed_suggested_models.is_empty() {
        return 0;
    }
    let normalized_target = current_provider_model.trim().to_ascii_lowercase();
    let target_model_id = current_provider_model
        .split_once(':')
        .map(|(_, model)| model.trim().to_ascii_lowercase())
        .unwrap_or_else(|| current_provider_model.trim().to_ascii_lowercase());

    if let Some(idx) = displayed_suggested_models.iter().position(|candidate| {
        let candidate_norm = candidate.trim().to_ascii_lowercase();
        if candidate_norm == normalized_target {
            return true;
        }
        if let Some((provider, model)) = candidate_norm.split_once(':') {
            if provider == selected_provider && model == target_model_id {
                return true;
            }
        }
        candidate_norm == target_model_id
    }) {
        return idx;
    }

    if selected_provider == "nous" {
        if let Some(idx) = displayed_suggested_models.iter().position(|candidate| {
            candidate
                .trim()
                .eq_ignore_ascii_case("moonshotai/kimi-k2.6")
        }) {
            return idx;
        }
    }

    0
}

fn setup_provider_display(provider: &str) -> &'static str {
    match provider {
        "openai" => "OpenAI",
        "openai-codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        "google-gemini-cli" => "Google Gemini CLI",
        "gemini" => "Google AI Studio",
        "openrouter" => "OpenRouter",
        "qwen" => "Alibaba DashScope",
        "alibaba" => "Alibaba Cloud DashScope",
        "qwen-oauth" => "Qwen OAuth",
        "alibaba-coding-plan" => "Alibaba Coding Plan",
        "deepseek" => "DeepSeek",
        "kimi-coding" => "Kimi Coding",
        "kimi-coding-cn" => "Kimi Coding CN",
        "minimax" => "MiniMax",
        "minimax-cn" => "MiniMax CN",
        "stepfun" => "StepFun",
        "nous" => "Nous",
        "ai-gateway" => "Vercel AI Gateway",
        "arcee" => "Arcee",
        "bedrock" => "AWS Bedrock",
        "copilot" => "GitHub Copilot",
        "huggingface" => "Hugging Face",
        "kilocode" => "KiloCode",
        "nvidia" => "NVIDIA NIM",
        "ollama-cloud" => "Ollama Cloud",
        "ollama-local" => "Ollama Local",
        "llama-cpp" => "llama.cpp Server",
        "vllm" => "vLLM Server",
        "mlx" => "MLX Server",
        "apple-ane" => "Apple ANE Endpoint",
        "sglang" => "SGLang Server",
        "tgi" => "Text Gen Inference",
        "opencode-go" => "OpenCode Go",
        "opencode-zen" => "OpenCode Zen",
        "xai" => "xAI",
        "xiaomi" => "Xiaomi MiMo",
        "zai" => "Z.AI / GLM",
        _ => "Provider",
    }
}

fn setup_provider_env_keys(provider: &str) -> &'static [&'static str] {
    match provider {
        "openai" => SETUP_OPENAI_ENV_KEYS,
        "anthropic" => SETUP_ANTHROPIC_ENV_KEYS,
        "openai-codex" => SETUP_OPENAI_CODEX_ENV_KEYS,
        "google-gemini-cli" => SETUP_GOOGLE_GEMINI_CLI_ENV_KEYS,
        "gemini" => SETUP_GEMINI_ENV_KEYS,
        "openrouter" => SETUP_OPENROUTER_ENV_KEYS,
        "qwen" | "alibaba" => SETUP_QWEN_ENV_KEYS,
        "qwen-oauth" => SETUP_QWEN_OAUTH_ENV_KEYS,
        "alibaba-coding-plan" => SETUP_ALIBABA_CODING_PLAN_ENV_KEYS,
        "deepseek" => SETUP_DEEPSEEK_ENV_KEYS,
        "kimi-coding" => SETUP_KIMI_CODING_ENV_KEYS,
        "kimi-coding-cn" => SETUP_KIMI_CODING_CN_ENV_KEYS,
        "minimax" => SETUP_MINIMAX_ENV_KEYS,
        "minimax-cn" => SETUP_MINIMAX_CN_ENV_KEYS,
        "stepfun" => SETUP_STEPFUN_ENV_KEYS,
        "nous" => SETUP_NOUS_ENV_KEYS,
        "ai-gateway" => SETUP_AI_GATEWAY_ENV_KEYS,
        "arcee" => SETUP_ARCEE_ENV_KEYS,
        "bedrock" => SETUP_BEDROCK_ENV_KEYS,
        "copilot" => SETUP_COPILOT_ENV_KEYS,
        "huggingface" => SETUP_HUGGINGFACE_ENV_KEYS,
        "kilocode" => SETUP_KILOCODE_ENV_KEYS,
        "nvidia" => SETUP_NVIDIA_ENV_KEYS,
        "ollama-cloud" => SETUP_OLLAMA_CLOUD_ENV_KEYS,
        "ollama-local" => SETUP_OLLAMA_LOCAL_ENV_KEYS,
        "llama-cpp" => SETUP_LLAMA_CPP_ENV_KEYS,
        "vllm" => SETUP_VLLM_ENV_KEYS,
        "mlx" => SETUP_MLX_ENV_KEYS,
        "apple-ane" => SETUP_APPLE_ANE_ENV_KEYS,
        "sglang" => SETUP_SGLANG_ENV_KEYS,
        "tgi" => SETUP_TGI_ENV_KEYS,
        "opencode-go" => SETUP_OPENCODE_GO_ENV_KEYS,
        "opencode-zen" => SETUP_OPENCODE_ZEN_ENV_KEYS,
        "xai" => SETUP_XAI_ENV_KEYS,
        "xiaomi" => SETUP_XIAOMI_ENV_KEYS,
        "zai" => SETUP_ZAI_ENV_KEYS,
        _ => &[],
    }
}

fn setup_provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "openai-codex" => Some("https://chatgpt.com/backend-api/codex"),
        "google-gemini-cli" => Some("cloudcode-pa://google"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta"),
        "qwen" | "alibaba" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        "alibaba-coding-plan" => Some("https://coding-intl.dashscope.aliyuncs.com/v1"),
        "deepseek" => Some("https://api.deepseek.com/v1"),
        "kimi-coding" => Some("https://api.moonshot.ai/v1"),
        "kimi-coding-cn" => Some("https://api.moonshot.cn/v1"),
        "minimax-cn" => Some("https://api.minimaxi.com/anthropic"),
        "stepfun" => Some("https://api.stepfun.ai/step_plan/v1"),
        "ai-gateway" => Some("https://ai-gateway.vercel.sh/v1"),
        "arcee" => Some("https://api.arcee.ai/api/v1"),
        "huggingface" => Some("https://router.huggingface.co/v1"),
        "kilocode" => Some("https://api.kilo.ai/api/gateway"),
        "nvidia" => Some("https://integrate.api.nvidia.com/v1"),
        "ollama-cloud" => Some("https://ollama.com/v1"),
        "ollama-local" => Some("http://127.0.0.1:11434/v1"),
        "llama-cpp" => Some("http://127.0.0.1:8080/v1"),
        "vllm" => Some("http://127.0.0.1:8000/v1"),
        "mlx" => Some("http://127.0.0.1:8080/v1"),
        "apple-ane" => Some("http://127.0.0.1:8081/v1"),
        "sglang" => Some("http://127.0.0.1:30000/v1"),
        "tgi" => Some("http://127.0.0.1:8082/v1"),
        "opencode-go" => Some("https://opencode.ai/zen/go/v1"),
        "opencode-zen" => Some("https://opencode.ai/zen/v1"),
        "xai" => Some("https://api.x.ai/v1"),
        "xiaomi" => Some("https://api.xiaomimimo.com/v1"),
        "zai" => Some("https://api.z.ai/api/paas/v4"),
        _ => None,
    }
}

fn setup_provider_requires_api_key(provider: &str) -> bool {
    !matches!(
        provider,
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi"
    )
}

fn local_backend_base_url_env_var(provider: &str) -> Option<&'static str> {
    match provider {
        "ollama-local" => Some("OLLAMA_BASE_URL"),
        "llama-cpp" => Some("LLAMA_CPP_BASE_URL"),
        "vllm" => Some("VLLM_BASE_URL"),
        "mlx" => Some("MLX_BASE_URL"),
        "apple-ane" => Some("APPLE_ANE_BASE_URL"),
        "sglang" => Some("SGLANG_BASE_URL"),
        "tgi" => Some("TGI_BASE_URL"),
        _ => None,
    }
}

fn merge_missing_env_keys(src: &Path, dst: &Path, label: &str) -> Result<usize, AgentError> {
    let src_content =
        read_env_text(src).map_err(|e| AgentError::Io(format!("read {}: {}", src.display(), e)))?;
    let existing = read_env_text(dst).unwrap_or_default();

    let existing_keys: std::collections::HashSet<String> = existing
        .lines()
        .filter_map(parse_env_assignment)
        .map(|(k, _)| k)
        .collect();

    let mut to_import = Vec::new();
    for line in src_content.lines() {
        if let Some((k, v)) = parse_env_assignment(line) {
            if existing_keys.contains(&k) {
                continue;
            }
            if normalize_env_value(&v).is_empty() {
                continue;
            }
            to_import.push(line.trim().to_string());
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

fn upsert_env_key(path: &Path, key: &str, value: &str) -> Result<(), AgentError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut updated_lines = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        if let Some((k, _)) = parse_env_assignment(line) {
            if k == key {
                updated_lines.push(format!("{key}={value}"));
                replaced = true;
                continue;
            }
        }
        updated_lines.push(line.to_string());
    }
    if !replaced {
        updated_lines.push(format!("{key}={value}"));
    }
    let mut updated = updated_lines.join("\n");
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    std::fs::write(path, updated)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
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

fn read_setup_stdin_line(stdin: &std::io::Stdin) -> String {
    use std::io::BufRead;
    let mut line = String::new();
    let mut reader = stdin.lock();
    reader.read_line(&mut line).ok();
    line
}

/// Handle `hermes setup`.
fn run_kanban(args: Vec<String>) -> Result<(), AgentError> {
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    println!("{}", hermes_cli::commands::run_kanban_command(&arg_refs)?);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortalActionKind {
    Setup,
    Info,
}

fn portal_action_kind(action: Option<&str>) -> Result<PortalActionKind, AgentError> {
    match action.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("setup" | "login" | "auth") => Ok(PortalActionKind::Setup),
        Some("info" | "status" | "check") => Ok(PortalActionKind::Info),
        Some(other) => Err(AgentError::Config(format!(
            "Unknown portal action '{other}'. Use `hermes portal` for setup or `hermes portal info` for status."
        ))),
    }
}

async fn run_portal(cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    match portal_action_kind(action.as_deref())? {
        PortalActionKind::Setup => {
            println!("Nous Portal setup ({DEFAULT_NOUS_PORTAL_URL})");
            run_auth(
                cli,
                Some("setup".to_string()),
                Some("nous".to_string()),
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        PortalActionKind::Info => {
            println!("Nous Portal info ({DEFAULT_NOUS_PORTAL_URL})");
            run_auth(
                cli,
                Some("status".to_string()),
                Some("nous".to_string()),
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
    }
}

async fn run_setup(cli: Cli) -> Result<(), AgentError> {
    use std::io::{self, Write};

    println!("Hermes Agent Ultra — Setup Wizard");
    println!("===========================\n");

    let config_dir = hermes_state_root(&cli);
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

    // 2. Optional import from legacy Python/OpenClaw .env files
    {
        let mut reader = stdin.lock();
        maybe_import_legacy_env(&mut reader, &env_path)?;
    }

    // 3. Choose setup depth first (upstream parity: quick/full first).
    let mode_labels = vec![
        "Quick setup (recommended) — provider, auth, model".to_string(),
        "Full setup — quick + personality + optional sections".to_string(),
    ];
    let mode_pick = hermes_cli::curses_select("Choose setup mode", &mode_labels, 0);
    let full_setup = mode_pick.confirmed && mode_pick.index == 1;

    // 4. Prompt for provider first (upstream parity: provider before model).
    let provider_defaults = setup_provider_defaults();
    let default_provider = SETUP_MODEL_OPTIONS
        .get(default_setup_model_choice().saturating_sub(1))
        .map(|option| option.provider)
        .unwrap_or("nous");
    let default_provider_index = provider_defaults
        .iter()
        .position(|option| option.provider == default_provider)
        .unwrap_or(0);
    let provider_labels: Vec<String> = provider_defaults
        .iter()
        .map(|option| {
            let auth_label = if provider_supports_oauth(option.provider) {
                "OAuth/API key"
            } else if !setup_provider_requires_api_key(option.provider) {
                "Local / optional key"
            } else if option.provider == "bedrock" {
                "AWS credentials"
            } else {
                "API key"
            };
            format!(
                "{:<22} {:<18} {}",
                setup_provider_display(option.provider),
                format!("({auth_label})"),
                option.label
            )
        })
        .collect();
    println!("\nSetup order: provider -> auth -> model.");
    let selected =
        hermes_cli::curses_select("Select provider", &provider_labels, default_provider_index);
    let selected_option = provider_defaults
        .get(selected.index)
        .unwrap_or(&provider_defaults[default_provider_index]);
    let mut model = selected_option.model.to_string();
    let selected_provider = selected_option.provider.to_string();
    let selected_provider_label = setup_provider_display(&selected_provider);
    let selected_provider_env_keys = setup_provider_env_keys(&selected_provider);
    let env_keys_display = selected_provider_env_keys.join("/");

    // 5. Prompt for selected provider API key (or OAuth device login where supported)
    let has_selected_provider_env_key = selected_provider_env_keys.iter().any(|key| {
        std::env::var(key)
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
            || read_env_key(&env_path, key).is_some()
    });
    let mut api_key = String::new();
    let mut stored_provider_secret_in_vault = false;
    let mut selected_base_url_override =
        setup_provider_default_base_url(&selected_provider).map(ToString::to_string);
    let mut selected_oauth_token_url: Option<String> = None;
    let mut selected_oauth_client_id: Option<String> = None;
    let mut selected_nous_oauth_authenticated = false;
    let mut selected_nous_managed_tools_enabled: Option<bool> = None;

    if provider_supports_oauth(&selected_provider) {
        print!(
            "\nAuthenticate with {} OAuth flow now? [Y/n]: ",
            selected_provider_label
        );
        io::stdout().flush().ok();
        let answer = read_setup_stdin_line(&stdin);
        let use_oauth = !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no");
        if use_oauth {
            let store = FileTokenStore::new(config_dir.join("auth").join("tokens.json")).await?;
            let manager = AuthManager::new(store);
            match selected_provider.as_str() {
                "nous" => {
                    let (resolved, auth_path, _imported_existing, state) =
                        resolve_or_fresh_login_nous(&manager, true).await?;
                    println!("  ✓ Saved Nous OAuth state: {}", auth_path.display());
                    selected_base_url_override = Some(resolved.base_url);
                    selected_oauth_token_url = Some(format!(
                        "{}/api/oauth/token",
                        if state.portal_base_url.trim().is_empty() {
                            DEFAULT_NOUS_PORTAL_URL
                        } else {
                            state.portal_base_url.trim_end_matches('/')
                        }
                    ));
                    selected_oauth_client_id = Some(if state.client_id.trim().is_empty() {
                        DEFAULT_NOUS_CLIENT_ID.to_string()
                    } else {
                        state.client_id.clone()
                    });
                    stored_provider_secret_in_vault = true;
                    selected_nous_oauth_authenticated = true;
                }
                "openai-codex" => {
                    let imported = discover_existing_openai_codex_oauth()?;
                    let state = if let Some(imported) = imported {
                        println!(
                            "  ✓ Detected existing OpenAI Codex OAuth session: {}",
                            imported.source_path.display()
                        );
                        imported.state
                    } else {
                        login_openai_codex_device_code(CodexDeviceCodeOptions::default()).await?
                    };
                    let auth_path = save_codex_auth_state(&state)?;
                    println!(
                        "  ✓ Saved OpenAI Codex OAuth state: {}",
                        auth_path.display()
                    );
                    manager
                        .save_credential(OAuthCredential {
                            provider: "openai-codex".to_string(),
                            access_token: state.tokens.access_token.clone(),
                            refresh_token: state.tokens.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: state
                                .tokens
                                .expires_in
                                .filter(|secs| *secs > 0)
                                .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs)),
                        })
                        .await?;
                    selected_oauth_token_url = Some(CODEX_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(CODEX_OAUTH_CLIENT_ID.to_string());
                    selected_base_url_override = Some(DEFAULT_CODEX_BASE_URL.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "openai" => {
                    let imported = discover_existing_openai_oauth()?;
                    let state = if let Some(imported) = imported {
                        println!(
                            "  ✓ Detected existing OpenAI OAuth session: {}",
                            imported.source_path.display()
                        );
                        imported.state
                    } else {
                        login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                    };
                    let auth_path = save_openai_auth_state(&state)?;
                    println!("  ✓ Saved OpenAI OAuth state: {}", auth_path.display());
                    manager
                        .save_credential(OAuthCredential {
                            provider: "openai".to_string(),
                            access_token: state.tokens.access_token.clone(),
                            refresh_token: state.tokens.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: state
                                .tokens
                                .expires_in
                                .filter(|secs| *secs > 0)
                                .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs)),
                        })
                        .await?;
                    selected_oauth_token_url = Some(CODEX_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(CODEX_OAUTH_CLIENT_ID.to_string());
                    selected_base_url_override = Some(DEFAULT_OPENAI_BASE_URL.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "anthropic" => {
                    let imported = discover_existing_anthropic_oauth()?;
                    let (state, source_label) = if let Some(imported) = imported {
                        println!(
                            "  ✓ Detected existing Anthropic OAuth session: {}",
                            imported.source_path.display()
                        );
                        (imported.state, imported.source)
                    } else {
                        (
                            login_anthropic_oauth(AnthropicOAuthLoginOptions::default()).await?,
                            "hermes_pkce".to_string(),
                        )
                    };
                    let auth_state = serde_json::json!({
                        "access_token": state.access_token.clone(),
                        "refresh_token": state.refresh_token.clone(),
                        "expires_at_ms": state.expires_at_ms,
                        "source": source_label,
                    });
                    let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                    println!("  ✓ Saved Anthropic OAuth state: {}", auth_path.display());
                    manager
                        .save_credential(OAuthCredential {
                            provider: "anthropic".to_string(),
                            access_token: state.access_token.clone(),
                            refresh_token: state.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: parse_unix_millis_utc(state.expires_at_ms),
                        })
                        .await?;
                    selected_oauth_token_url = Some(ANTHROPIC_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(ANTHROPIC_OAUTH_CLIENT_ID.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "qwen-oauth" => {
                    let creds = resolve_qwen_runtime_credentials(
                        false,
                        true,
                        QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                    )
                    .await?;
                    let auth_state = serde_json::to_value(&creds.tokens)
                        .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                    let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                    println!("  ✓ Saved Qwen OAuth state: {}", auth_path.display());
                    manager
                        .save_credential(OAuthCredential {
                            provider: "qwen-oauth".to_string(),
                            access_token: creds.api_key.clone(),
                            refresh_token: creds.refresh_token.clone(),
                            token_type: creds.token_type.clone(),
                            scope: None,
                            expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                        })
                        .await?;
                    selected_base_url_override = Some(creds.base_url.clone());
                    selected_oauth_token_url = Some(QWEN_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(QWEN_OAUTH_CLIENT_ID.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "google-gemini-cli" => {
                    let creds =
                        login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default()).await?;
                    let auth_state = serde_json::json!({
                        "access_token": creds.api_key.clone(),
                        "refresh_token": creds.refresh_token.clone(),
                        "expires_at_ms": creds.expires_at_ms,
                        "email": creds.email.clone(),
                        "project_id": creds.project_id.clone(),
                        "source": creds.source.clone(),
                    });
                    let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                    println!(
                        "  ✓ Saved Google Gemini OAuth state: {}",
                        auth_path.display()
                    );
                    manager
                        .save_credential(OAuthCredential {
                            provider: "google-gemini-cli".to_string(),
                            access_token: creds.api_key.clone(),
                            refresh_token: creds.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                        })
                        .await?;
                    selected_base_url_override = Some(creds.base_url.clone());
                    stored_provider_secret_in_vault = true;
                }
                _ => {}
            }
        }
    }

    if selected_provider == "nous" {
        if selected_nous_oauth_authenticated {
            print!("\nEnable Nous managed tool-gateway integrations (recommended) [Y/n]: ");
            io::stdout().flush().ok();
            let answer = read_setup_stdin_line(&stdin);
            let enable = !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no");
            selected_nous_managed_tools_enabled = Some(enable);
        } else {
            println!(
                "\nNote: Nous managed tool-gateway integrations require Nous OAuth login in setup."
            );
            println!(
                "      Re-run setup with Nous OAuth, then set {}=1 if needed.",
                HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY
            );
        }
    }

    if selected_provider == "bedrock" {
        println!(
            "\nAWS Bedrock uses AWS credential chain (env/profile/role). Skipping API key prompt."
        );
    } else if !setup_provider_requires_api_key(&selected_provider) {
        println!(
            "\n{} is a local/self-host OpenAI-compatible backend. API key is optional.",
            selected_provider_label
        );
        if has_selected_provider_env_key {
            print!(
                "{} API key (optional, leave blank to keep {} from environment/{}): ",
                selected_provider_label,
                env_keys_display,
                env_path.display()
            );
            io::stdout().flush().ok();
            api_key = read_setup_stdin_line(&stdin).trim().to_string();
        }
    } else if !stored_provider_secret_in_vault {
        if has_selected_provider_env_key {
            print!(
                "\n{} API key (leave blank to keep {} from environment/{}): ",
                selected_provider_label,
                env_keys_display,
                env_path.display()
            );
        } else {
            print!(
                "\n{} API key (leave blank to skip): ",
                selected_provider_label
            );
        }
        io::stdout().flush().ok();
        api_key = read_setup_stdin_line(&stdin).trim().to_string();
    }

    if !api_key.is_empty() {
        print!(
            "Store {} key in encrypted vault (recommended) [Y/n]: ",
            selected_provider_label
        );
        io::stdout().flush().ok();
        let answer = read_setup_stdin_line(&stdin);
        let use_vault = !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no");
        if use_vault {
            let store = FileTokenStore::new(config_dir.join("auth").join("tokens.json")).await?;
            let manager = AuthManager::new(store);
            manager
                .save_credential(OAuthCredential {
                    provider: selected_provider.clone(),
                    access_token: api_key.clone(),
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            stored_provider_secret_in_vault = true;
        }
    }

    // 6. Prompt for model after provider auth is established.
    let suggested_provider_models = provider_model_ids(&selected_provider).await;
    let suggested_limit = if selected_provider == "nous" {
        usize::MAX
    } else {
        25
    };
    let displayed_suggested_models: Vec<String> = suggested_provider_models
        .into_iter()
        .take(suggested_limit)
        .collect();
    if displayed_suggested_models.is_empty() {
        print!("Model ID for {} [{}]: ", selected_provider_label, model);
        io::stdout().flush().ok();
        let model_override = read_setup_stdin_line(&stdin);
        let model_override = model_override.trim();
        if !model_override.is_empty() {
            let candidate = if model_override.contains(':') {
                model_override.to_string()
            } else {
                format!("{}:{}", selected_provider, model_override)
            };
            model = normalize_provider_model(&candidate)?;
        }
    } else {
        let mut suggested_labels: Vec<String> = displayed_suggested_models
            .iter()
            .map(|candidate| {
                if candidate.contains(':') {
                    candidate.to_string()
                } else {
                    format!("{}:{}", selected_provider, candidate)
                }
            })
            .collect();
        suggested_labels.push("Custom model ID…".to_string());
        let model_title = if selected_provider == "nous" {
            format!(
                "Select {} model ({} available)",
                selected_provider_label,
                displayed_suggested_models.len()
            )
        } else {
            format!("Select {} model", selected_provider_label)
        };
        let default_model_index =
            setup_default_model_pick_index(&selected_provider, &model, &displayed_suggested_models);
        let suggested_pick =
            hermes_cli::curses_select(&model_title, &suggested_labels, default_model_index);
        if suggested_pick.confirmed && suggested_pick.index < displayed_suggested_models.len() {
            let candidate = &displayed_suggested_models[suggested_pick.index];
            model = if candidate.contains(':') {
                candidate.to_string()
            } else {
                format!("{}:{}", selected_provider, candidate)
            };
        } else if suggested_pick.confirmed {
            print!(
                "Custom model ID for {} (provider prefix optional) [{}]: ",
                selected_provider_label, model
            );
            io::stdout().flush().ok();
            let model_override = read_setup_stdin_line(&stdin);
            let model_override = model_override.trim();
            if !model_override.is_empty() {
                let candidate = if model_override.contains(':') {
                    model_override.to_string()
                } else {
                    format!("{}:{}", selected_provider, model_override)
                };
                model = normalize_provider_model(&candidate)?;
            }
        }
    }

    // 7. Prompt for personality (full setup only).
    let personality = if full_setup {
        let builtin_personalities = hermes_agent::builtin_personality_names();
        let builtin_descriptions = hermes_agent::builtin_personality_descriptions();
        println!("\nBuilt-in personality guide:");
        for (name, usage) in builtin_descriptions {
            println!("  - {:<14} {}", name, usage);
        }
        print!(
            "\nPersonality (default, {}) [default]: ",
            builtin_personalities.join(", ")
        );
        io::stdout().flush().ok();
        let personality = read_setup_stdin_line(&stdin);
        let personality = personality.trim();
        if personality.is_empty() {
            "default".to_string()
        } else {
            if !personality.contains(char::is_whitespace)
                && !personality.eq_ignore_ascii_case("default")
                && !builtin_personalities
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(personality))
            {
                println!(
                    "  ! '{}' is not built-in. Hermes will look for personalities/{}.md.",
                    personality, personality
                );
            }
            personality.to_string()
        }
    } else {
        println!("\nQuick setup: using default personality.");
        "default".to_string()
    };

    // 8. Write config.yaml
    let mut overwrite_config = true;
    if config_path.exists() {
        print!("\nconfig.yaml already exists. Overwrite? [y/N]: ");
        io::stdout().flush().ok();
        let answer = read_setup_stdin_line(&stdin);
        if !answer.trim().eq_ignore_ascii_case("y") {
            overwrite_config = false;
            println!("Keeping existing config.yaml.");
        }
    }

    // Preserve existing fields (including platform_toolsets) instead of
    // rewriting config.yaml from scratch.
    let mut disk =
        load_user_config_file(&config_path).map_err(|e| AgentError::Config(e.to_string()))?;
    if overwrite_config {
        disk.model = Some(model.clone());
        disk.personality = Some(personality.to_string());
        disk.max_turns = 250;

        let _ = upsert_env_key(
            &env_path,
            "HERMES_AUTH_DEFAULT_PROVIDER",
            selected_provider.as_str(),
        );

        if !api_key.is_empty() && !stored_provider_secret_in_vault {
            let provider = disk
                .llm_providers
                .entry(selected_provider.clone())
                .or_insert_with(hermes_config::LlmProviderConfig::default);
            provider.api_key = Some(api_key.clone());
        } else if stored_provider_secret_in_vault {
            println!(
                "  ✓ Stored {} key in encrypted vault: {}",
                selected_provider_label,
                config_dir.join("auth").join("tokens.json").display()
            );
        } else if has_selected_provider_env_key {
            println!(
                "  ✓ Keeping {} from environment/{} for runtime auth",
                env_keys_display,
                env_path.display(),
            );
        }
        let provider = disk
            .llm_providers
            .entry(selected_provider.clone())
            .or_insert_with(hermes_config::LlmProviderConfig::default);
        if let Some(base_url) = selected_base_url_override {
            provider.base_url = Some(base_url);
        }
        if let Some(token_url) = selected_oauth_token_url {
            provider.oauth_token_url = Some(token_url);
        }
        if let Some(client_id) = selected_oauth_client_id {
            provider.oauth_client_id = Some(client_id);
        }
        validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
        save_config_yaml(&config_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
        println!("\n  ✓ Wrote config.yaml");
    }

    if let Some(enabled) = selected_nous_managed_tools_enabled {
        let flag = if enabled { "1" } else { "0" };
        upsert_env_key(&env_path, HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY, flag)?;
        println!("  ✓ {}={}", HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY, flag);
    }

    // 6. Write default profile
    let default_profile = config_dir.join("profiles").join("default.yaml");
    if !default_profile.exists() {
        let profile_model = disk.model.clone().unwrap_or_else(|| model.clone());
        let profile_personality = disk
            .personality
            .clone()
            .unwrap_or_else(|| personality.to_string());
        let profile_content = format!(
            "# Default Hermes Profile\nname: default\nmodel: {}\npersonality: {}\n",
            profile_model, profile_personality,
        );
        std::fs::write(&default_profile, profile_content)
            .map_err(|e| AgentError::Io(format!("Failed to write profile: {}", e)))?;
        println!("  ✓ Created default profile");
    }

    // 7. Ensure SOUL.md exists so users can customize persona immediately.
    let soul_path = config_dir.join("SOUL.md");
    if !soul_path.exists() {
        let soul_template = "# Hermes Agent Persona\n\n<!--\nCustomize this file to control how Hermes communicates.\nThis file is loaded every message; no restart needed.\nDelete this file (or leave it empty) to use the default personality.\n-->\n";
        std::fs::write(&soul_path, soul_template)
            .map_err(|e| AgentError::Io(format!("Failed to write SOUL.md: {}", e)))?;
        println!("  ✓ Created SOUL.md");
    }

    if full_setup && prompt_yes_no("\nConfigure optional setup sections now?", true).await? {
        run_optional_setup_sections(&cli, &disk).await?;
    } else if !full_setup {
        println!("Skipped optional setup sections (quick setup mode).");
    }

    println!(
        "\nSetup complete! Run `hermes-ultra` (or `hermes-agent-ultra`/`hermes`) to start an interactive session."
    );
    println!(
        "Run `hermes-ultra doctor` (or `hermes-agent-ultra doctor`/`hermes doctor`) to check system requirements."
    );
    Ok(())
}

async fn run_update(
    check: bool,
    yes: bool,
    rollback: bool,
    force: bool,
    source: Option<String>,
    channel: Option<String>,
) -> Result<(), AgentError> {
    // Clean up leftover .old files from previous update
    hermes_cli::update::replace::cleanup_old();

    if rollback {
        return hermes_cli::update::replace::rollback();
    }

    if check {
        println!("Hermes Agent v{}", env!("CARGO_PKG_VERSION"));
        println!("{}", hermes_cli::update::check_for_updates().await?);
        return Ok(());
    }

    // Perform full OTA update
    hermes_cli::update::perform_update(hermes_cli::update::UpdateOptions {
        yes,
        force,
        source,
        channel,
    })
    .await
}

async fn run_elite_check(_cli: Cli, json: bool, strict: bool) -> Result<(), AgentError> {
    let base_cmd = std::env::var("HERMES_ELITE_GATE_CMD")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "python3 scripts/run-elite-sync-gate.py --repo-root .".to_string());
    let mut cmdline = base_cmd;
    if json {
        cmdline.push_str(" --json");
    }
    let output = tokio::process::Command::new("bash")
        .args(["-lc", &cmdline])
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("elite-check command failed to start: {}", e)))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        println!("{}", stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        eprintln!("{}", stderr.trim_end());
    }
    if strict && !output.status.success() {
        return Err(AgentError::Config(format!(
            "elite-check failed (status={})",
            output.status
        )));
    }
    Ok(())
}

async fn run_verify_provenance(
    cli: Cli,
    path: String,
    signature: Option<String>,
    strict: bool,
    json: bool,
) -> Result<(), AgentError> {
    let artifact = PathBuf::from(path);
    let signature_path = signature
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| Some(provenance_sidecar_path_for_artifact(&artifact)));
    let verification = verify_artifact_provenance(
        &hermes_state_root(&cli),
        &artifact,
        signature_path.as_deref(),
    )?;
    let rendered = if json {
        serde_json::to_string(&verification)
            .map_err(|e| AgentError::Config(format!("serialize verification: {}", e)))?
    } else {
        serde_json::to_string_pretty(&verification)
            .map_err(|e| AgentError::Config(format!("serialize verification: {}", e)))?
    };
    if verification.ok {
        if !json {
            println!("Provenance verification: ✓");
        }
        println!("{rendered}");
        return Ok(());
    }
    if !json {
        println!("Provenance verification: ✗");
    }
    println!("{rendered}");
    if strict {
        return Err(AgentError::Config(
            verification.reason.clone().unwrap_or_else(|| {
                format!("provenance verification failed ({})", verification.code)
            }),
        ));
    }
    Ok(())
}

async fn run_rotate_provenance_key(cli: Cli, json: bool) -> Result<(), AgentError> {
    let path = provenance_key_path_for_cli(&hermes_state_root(&cli));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }

    let archived_path = if path.exists() {
        let archived = path.with_file_name(format!(
            "provenance.key.{}.bak",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::rename(&path, &archived)
            .map_err(|e| AgentError::Io(format!("archive {}: {}", path.display(), e)))?;
        Some(archived)
    } else {
        None
    };

    let mut key_bytes = [0u8; 32];
    {
        use rand::TryRng;
        rand::rngs::SysRng
            .try_fill_bytes(&mut key_bytes)
            .map_err(|e| AgentError::Config(e.to_string()))?;
    }
    let key_hex = hex::encode(key_bytes);
    std::fs::write(&path, format!("{key_hex}\n"))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .map_err(|e| AgentError::Io(format!("metadata {}: {}", path.display(), e)))?
            .permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }

    let key_id = {
        let digest = Sha256::digest(key_bytes);
        let full = hex::encode(digest);
        full.chars().take(16).collect::<String>()
    };
    let payload = serde_json::json!({
        "ok": true,
        "key_path": path.display().to_string(),
        "key_id": key_id,
        "archived_previous_key": archived_path.as_ref().map(|p| p.display().to_string()),
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize rotate response: {}", e)))?
        );
    } else {
        println!("Rotated provenance signing key.");
        println!("Active key: {}", path.display());
        if let Some(prev) = archived_path {
            println!("Archived previous key: {}", prev.display());
        }
        println!("New key id: {}", key_id);
    }
    Ok(())
}

/// Handle `hermes status`.
async fn run_status(cli: Cli) -> Result<(), AgentError> {
    println!("Hermes Agent Ultra — Status");
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

    let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "enforce".to_string());
    let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "relaxed".to_string());
    let policy_counters_path = default_tool_policy_counters_path();
    let policy_counters = load_tool_policy_counters(&policy_counters_path).unwrap_or_default();
    println!(
        "Tool policy: mode={} preset={} counters(allow={}, deny={}, audit={}, simulate={}, would_block={})",
        policy_mode,
        policy_preset,
        policy_counters.allow,
        policy_counters.deny,
        policy_counters.audit_only,
        policy_counters.simulate,
        policy_counters.would_block,
    );

    let route_health_path = route_health_state_path_for_cli(&hermes_state_root(&cli));
    if route_health_path.exists() {
        match std::fs::read_to_string(&route_health_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        {
            Some(v) => {
                let summary = v.get("summary").cloned().unwrap_or_default();
                println!(
                    "Route health: overall={} entries={} avg_score={:.3}",
                    summary
                        .get("overall")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown"),
                    summary.get("entries").and_then(|x| x.as_u64()).unwrap_or(0),
                    summary
                        .get("average_score")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0),
                );
            }
            None => {
                println!(
                    "Route health: unavailable (failed to parse {})",
                    route_health_path.display()
                );
            }
        }
    } else {
        println!("Route health: (not generated) run `hermes route-health` to compute.");
    }

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
fn try_open_url(url: &str) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    cmd.arg(url);

    let status = cmd
        .status()
        .map_err(|e| AgentError::Io(format!("open browser command failed: {}", e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(AgentError::Io(format!(
            "open browser command exited with status {}",
            status
        )))
    }
}

async fn run_dashboard(
    cli: Cli,
    host: String,
    port: u16,
    no_open: bool,
    insecure: bool,
) -> Result<(), AgentError> {
    let host_trimmed = host.trim().to_string();
    let local_host = matches!(host_trimmed.as_str(), "127.0.0.1" | "localhost" | "::1");
    if !local_host && !insecure {
        return Err(AgentError::Config(
            "dashboard refused non-localhost bind without --insecure".into(),
        ));
    }

    let cfg_path = hermes_state_root(&cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    let api = disk
        .platforms
        .entry("api_server".to_string())
        .or_insert_with(PlatformConfig::default);
    api.enabled = true;
    api.extra.insert(
        "host".to_string(),
        serde_json::Value::String(host_trimmed.clone()),
    );
    api.extra
        .insert("port".to_string(), serde_json::Value::Number(port.into()));
    validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
    save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;

    let display_host = if host_trimmed == "0.0.0.0" {
        "127.0.0.1"
    } else {
        host_trimmed.as_str()
    };
    let url = format!("http://{}:{}/", display_host, port);
    println!(
        "Dashboard config written to {} (api_server enabled).",
        cfg_path.display()
    );
    println!("Dashboard URL: {}", url);

    if !no_open {
        let url_for_open = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            if let Err(e) = try_open_url(&url_for_open) {
                eprintln!("Dashboard auto-open failed: {}", e);
            }
        });
    }

    run_gateway(
        cli,
        Some("run".to_string()),
        false,
        false,
        false,
        None,
        false,
        false,
        false,
        false,
    )
    .await
}

async fn run_debug(
    cli: Cli,
    action: Option<String>,
    url: Option<String>,
    lines: u32,
    expire: u32,
    local: bool,
) -> Result<(), AgentError> {
    let reports_dir = debug_reports_dir_for_cli(&cli);
    std::fs::create_dir_all(&reports_dir)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", reports_dir.display(), e)))?;
    let now_unix = chrono::Utc::now().timestamp();
    let pending_removed = best_effort_sweep_expired_pending_pastes(&reports_dir, now_unix);
    if pending_removed > 0 {
        println!(
            "Pruned {} expired pending paste record(s).",
            pending_removed
        );
    }
    let removed = prune_old_debug_reports(&reports_dir, expire)?;
    if removed > 0 {
        println!(
            "Pruned {} expired debug report(s) older than {} day(s).",
            removed, expire
        );
    }

    match action.as_deref().unwrap_or("share") {
        "share" => {
            let report = collect_debug_report(&cli, lines)?;
            let filename = format!(
                "{}-debug-report.md",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
            );
            let path = reports_dir.join(filename);
            std::fs::write(&path, &report)
                .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
            println!("Debug report saved: {}", path.display());
            if local {
                println!("{}", report);
                return Ok(());
            }

            match reqwest::Client::new()
                .post("https://paste.rs")
                .header("Content-Type", "text/plain; charset=utf-8")
                .body(report)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let body = resp.text().await.unwrap_or_default();
                    println!("Shared debug report URL: {}", body.trim());
                    let _ = record_pending_paste(
                        &reports_dir,
                        body.trim(),
                        expire,
                        chrono::Utc::now().timestamp(),
                    );
                }
                Ok(resp) => {
                    println!(
                        "Debug share upload failed with status {}. Local report kept at {}",
                        resp.status(),
                        path.display()
                    );
                }
                Err(e) => {
                    println!(
                        "Debug share upload failed: {}. Local report kept at {}",
                        e,
                        path.display()
                    );
                }
            }
        }
        "delete" => {
            let target = url.ok_or_else(|| {
                AgentError::Config(
                    "debug delete requires a local report path or file:// URL".into(),
                )
            })?;
            let path = if let Some(rest) = target.strip_prefix("file://") {
                PathBuf::from(rest)
            } else {
                PathBuf::from(&target)
            };
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
                println!("Removed debug report {}", path.display());
            } else {
                println!("Debug report not found: {}", path.display());
            }
        }
        "list" => {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&reports_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.is_file())
                        .collect()
                })
                .unwrap_or_default();
            entries.sort();
            if entries.is_empty() {
                println!("No debug reports in {}", reports_dir.display());
            } else {
                println!("Debug reports ({}):", reports_dir.display());
                for p in entries {
                    println!("  {}", p.display());
                }
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown debug action '{}'. Use share|delete|list",
                other
            )));
        }
    }
    Ok(())
}

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

fn profile_aliases_path(profiles_dir: &Path) -> PathBuf {
    profiles_dir.join("aliases.json")
}

fn active_profile_marker_path(profiles_dir: &Path) -> PathBuf {
    profiles_dir.join(".active_profile")
}

fn load_profile_aliases(
    path: &Path,
) -> Result<std::collections::BTreeMap<String, String>, AgentError> {
    if !path.exists() {
        return Ok(std::collections::BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_profile_aliases(
    path: &Path,
    aliases: &std::collections::BTreeMap<String, String>,
) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw =
        serde_json::to_string_pretty(aliases).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn resolve_profile_name(
    requested: &str,
    aliases: &std::collections::BTreeMap<String, String>,
) -> String {
    aliases
        .get(requested.trim())
        .cloned()
        .unwrap_or_else(|| requested.trim().to_string())
}

fn resolve_profile_yaml_path(profiles_dir: &Path, name: &str) -> Option<PathBuf> {
    let yaml = profiles_dir.join(format!("{}.yaml", name));
    if yaml.exists() {
        return Some(yaml);
    }
    let yml = profiles_dir.join(format!("{}.yml", name));
    if yml.exists() {
        return Some(yml);
    }
    None
}

fn read_active_profile_name(profiles_dir: &Path) -> Option<String> {
    std::fs::read_to_string(active_profile_marker_path(profiles_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_active_profile_name(profiles_dir: &Path, name: &str) -> Result<(), AgentError> {
    let path = active_profile_marker_path(profiles_dir);
    std::fs::create_dir_all(profiles_dir)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", profiles_dir.display(), e)))?;
    std::fs::write(&path, format!("{}\n", name.trim()))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn load_profile_yaml(path: &Path) -> Result<serde_yaml::Value, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_yaml::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_profile_yaml(path: &Path, value: &serde_yaml::Value) -> Result<(), AgentError> {
    let raw = serde_yaml::to_string(value).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn validate_profile_name(name: &str) -> Result<String, AgentError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AgentError::Config("profile name cannot be empty".into()));
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains('/') || trimmed.contains('\\') {
        return Err(AgentError::Config(format!(
            "invalid profile name '{}': path separators are not allowed",
            trimmed
        )));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(AgentError::Config(format!(
            "invalid profile name '{}': use letters, numbers, '-', '_' or '.'",
            trimmed
        )));
    }
    Ok(trimmed.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn run_profile(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    secondary: Option<String>,
    output: Option<String>,
    import_name: Option<String>,
    alias_name: Option<String>,
    remove: bool,
    yes: bool,
    clone: bool,
    clone_all: bool,
    clone_from: Option<String>,
    no_alias: bool,
    no_skills: bool,
) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let profiles_dir = config_dir.join("profiles");
    let aliases_path = profile_aliases_path(&profiles_dir);
    let mut aliases = load_profile_aliases(&aliases_path)?;

    match action.as_deref().unwrap_or("show") {
        "show" => {
            if let Some(requested) = name {
                let resolved = resolve_profile_name(&requested, &aliases);
                let Some(path) = resolve_profile_yaml_path(&profiles_dir, &resolved) else {
                    return Err(AgentError::Config(format!(
                        "profile '{}' not found (resolved to '{}')",
                        requested, resolved
                    )));
                };
                let raw = std::fs::read_to_string(&path)
                    .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
                println!("{}", raw);
                return Ok(());
            }
            let config = load_config(cli.config_dir.as_deref())
                .map_err(|e| AgentError::Config(e.to_string()))?;
            let active =
                read_active_profile_name(&profiles_dir).unwrap_or_else(|| "(none)".to_string());
            println!("Current profile:");
            println!("  Active:      {}", active);
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
        "list" => {
            if !profiles_dir.exists() {
                println!("No profiles directory found. Run `hermes setup` first.");
                return Ok(());
            }
            let active = read_active_profile_name(&profiles_dir);
            let mut entries: Vec<String> = std::fs::read_dir(&profiles_dir)
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
            entries.sort();

            if entries.is_empty() {
                println!("No profiles found. Create one with `hermes profile create <name>`.");
            } else {
                println!("Available profiles:");
                for name in &entries {
                    let marker = if active.as_deref() == Some(name.as_str()) {
                        "*"
                    } else {
                        " "
                    };
                    println!("{} {}", marker, name);
                }
                if !aliases.is_empty() {
                    println!("\nAliases:");
                    for (alias, target) in &aliases {
                        println!("  {} -> {}", alias, target);
                    }
                }
            }
        }
        "create" => {
            let profile_name = name.ok_or_else(|| {
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile create <name>".into(),
                )
            })?;
            let profile_name = validate_profile_name(&profile_name)?;

            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("Failed to create profiles dir: {}", e)))?;

            let profile_path = profiles_dir.join(format!("{}.yaml", profile_name));
            if profile_path.exists() {
                return Err(AgentError::Config(format!(
                    "Profile '{}' already exists at {}",
                    profile_name,
                    profile_path.display()
                )));
            }

            let source_name = clone_from
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| resolve_profile_name(s, &aliases))
                .or_else(|| read_active_profile_name(&profiles_dir));
            let source_value = if clone || clone_all {
                let src = source_name.clone().ok_or_else(|| {
                    AgentError::Config(
                        "profile create --clone/--clone-all requires --clone-from or an active profile"
                            .into(),
                    )
                })?;
                let src_path = resolve_profile_yaml_path(&profiles_dir, &src).ok_or_else(|| {
                    AgentError::Config(format!("clone source profile '{}' not found", src))
                })?;
                Some(load_profile_yaml(&src_path)?)
            } else {
                None
            };

            let mut out_map = serde_yaml::Mapping::new();
            out_map.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(profile_name.clone()),
            );

            if let Some(src) = source_value {
                if let Some(src_map) = src.as_mapping() {
                    if clone_all {
                        out_map = src_map.clone();
                        out_map.insert(
                            serde_yaml::Value::String("name".to_string()),
                            serde_yaml::Value::String(profile_name.clone()),
                        );
                    } else {
                        for key in ["model", "personality", "max_turns"] {
                            let k = serde_yaml::Value::String(key.to_string());
                            if let Some(v) = src_map.get(&k) {
                                out_map.insert(k, v.clone());
                            }
                        }
                    }
                }
            }

            if no_skills {
                let skills_key = serde_yaml::Value::String("skills".to_string());
                let overrides_key = serde_yaml::Value::String("skill_overrides".to_string());
                out_map.remove(&skills_key);
                out_map.remove(&overrides_key);
            }

            out_map
                .entry(serde_yaml::Value::String("model".to_string()))
                .or_insert_with(|| serde_yaml::Value::String("openai:gpt-4o".to_string()));
            out_map
                .entry(serde_yaml::Value::String("personality".to_string()))
                .or_insert_with(|| serde_yaml::Value::String("default".to_string()));
            out_map
                .entry(serde_yaml::Value::String("max_turns".to_string()))
                .or_insert_with(|| serde_yaml::Value::Number(serde_yaml::Number::from(50u64)));

            save_profile_yaml(&profile_path, &serde_yaml::Value::Mapping(out_map))?;
            println!(
                "Created profile '{}' at {}",
                profile_name,
                profile_path.display()
            );

            if !no_alias {
                if let Some(alias) = alias_name
                    .or(secondary)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    aliases.insert(alias.clone(), profile_name.clone());
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Added alias '{}' -> '{}'.", alias, profile_name);
                }
            }
        }
        "use" | "switch" => {
            let requested = name.ok_or_else(|| {
                AgentError::Config("Missing profile name. Usage: hermes profile use <name>".into())
            })?;
            let resolved = resolve_profile_name(&requested, &aliases);
            let path = resolve_profile_yaml_path(&profiles_dir, &resolved).ok_or_else(|| {
                AgentError::Config(format!(
                    "Profile '{}' not found (resolved to '{}')",
                    requested, resolved
                ))
            })?;
            let value = load_profile_yaml(&path)?;
            let mut disk = load_user_config_file(&config_dir.join("config.yaml"))
                .map_err(|e| AgentError::Config(e.to_string()))?;
            if let Some(map) = value.as_mapping() {
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("model".to_string()))
                    .and_then(|v| v.as_str())
                {
                    disk.model = Some(v.to_string());
                }
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("personality".to_string()))
                    .and_then(|v| v.as_str())
                {
                    disk.personality = Some(v.to_string());
                }
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("max_turns".to_string()))
                    .and_then(|v| v.as_u64())
                {
                    disk.max_turns = v.min(u32::MAX as u64) as u32;
                }
            }
            save_config_yaml(&config_dir.join("config.yaml"), &disk)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            write_active_profile_name(&profiles_dir, &resolved)?;
            println!(
                "Activated profile '{}' (requested '{}').",
                resolved, requested
            );
        }
        "delete" => {
            let requested = name.ok_or_else(|| {
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile delete <name>".into(),
                )
            })?;
            let resolved = resolve_profile_name(&requested, &aliases);
            let path = resolve_profile_yaml_path(&profiles_dir, &resolved).ok_or_else(|| {
                AgentError::Config(format!(
                    "Profile '{}' not found (resolved to '{}')",
                    requested, resolved
                ))
            })?;
            if !yes
                && !prompt_yes_no(
                    &format!("Delete profile '{}' ({})?", resolved, path.display()),
                    false,
                )
                .await?
            {
                println!("Aborted.");
                return Ok(());
            }
            std::fs::remove_file(&path)
                .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
            aliases.retain(|alias, target| alias != &requested && target != &resolved);
            save_profile_aliases(&aliases_path, &aliases)?;
            if read_active_profile_name(&profiles_dir).as_deref() == Some(resolved.as_str()) {
                let _ = std::fs::remove_file(active_profile_marker_path(&profiles_dir));
            }
            println!("Deleted profile '{}' ({})", resolved, path.display());
        }
        "alias" => {
            if remove {
                let alias = alias_name
                    .or(name)
                    .or(secondary)
                    .ok_or_else(|| AgentError::Config("profile alias --remove <alias>".into()))?;
                if aliases.remove(alias.trim()).is_some() {
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Removed alias '{}'.", alias.trim());
                } else {
                    println!("Alias '{}' not found.", alias.trim());
                }
                return Ok(());
            }
            let target = name.ok_or_else(|| {
                AgentError::Config(
                    "profile alias usage: hermes profile alias <target> --name <alias>".into(),
                )
            })?;
            let alias = alias_name.or(secondary).ok_or_else(|| {
                AgentError::Config(
                    "profile alias usage: hermes profile alias <target> --name <alias>".into(),
                )
            })?;
            let resolved_target = resolve_profile_name(&target, &aliases);
            if resolve_profile_yaml_path(&profiles_dir, &resolved_target).is_none() {
                return Err(AgentError::Config(format!(
                    "Alias target profile '{}' not found",
                    resolved_target
                )));
            }
            aliases.insert(alias.trim().to_string(), resolved_target.clone());
            save_profile_aliases(&aliases_path, &aliases)?;
            println!("Alias '{}' -> '{}'", alias.trim(), resolved_target);
        }
        "rename" => {
            let old_requested = name.ok_or_else(|| {
                AgentError::Config("profile rename usage: hermes profile rename <old> <new>".into())
            })?;
            let new_name = secondary.ok_or_else(|| {
                AgentError::Config("profile rename usage: hermes profile rename <old> <new>".into())
            })?;
            let new_name = validate_profile_name(&new_name)?;
            let old_resolved = resolve_profile_name(&old_requested, &aliases);
            let old_path =
                resolve_profile_yaml_path(&profiles_dir, &old_resolved).ok_or_else(|| {
                    AgentError::Config(format!("Profile '{}' not found", old_resolved))
                })?;
            let new_path = profiles_dir.join(format!("{}.yaml", new_name));
            if new_path.exists() {
                return Err(AgentError::Config(format!(
                    "Target profile '{}' already exists",
                    new_name
                )));
            }
            std::fs::rename(&old_path, &new_path).map_err(|e| {
                AgentError::Io(format!(
                    "rename {} -> {}: {}",
                    old_path.display(),
                    new_path.display(),
                    e
                ))
            })?;
            if let Ok(mut value) = load_profile_yaml(&new_path) {
                if let Some(map) = value.as_mapping_mut() {
                    map.insert(
                        serde_yaml::Value::String("name".to_string()),
                        serde_yaml::Value::String(new_name.clone()),
                    );
                    let _ = save_profile_yaml(&new_path, &value);
                }
            }
            for target in aliases.values_mut() {
                if target == &old_resolved {
                    *target = new_name.clone();
                }
            }
            if let Some(v) = aliases.remove(&old_requested) {
                aliases.insert(
                    new_name.clone(),
                    if v == old_resolved {
                        new_name.clone()
                    } else {
                        v
                    },
                );
            }
            save_profile_aliases(&aliases_path, &aliases)?;
            if read_active_profile_name(&profiles_dir).as_deref() == Some(old_resolved.as_str()) {
                write_active_profile_name(&profiles_dir, &new_name)?;
            }
            println!("Renamed profile '{}' -> '{}'", old_resolved, new_name);
        }
        "export" => {
            let target = if let Some(n) = name {
                resolve_profile_name(&n, &aliases)
            } else {
                read_active_profile_name(&profiles_dir).ok_or_else(|| {
                    AgentError::Config(
                        "profile export: no active profile and no name provided".into(),
                    )
                })?
            };
            let source = resolve_profile_yaml_path(&profiles_dir, &target)
                .ok_or_else(|| AgentError::Config(format!("Profile '{}' not found", target)))?;
            let out = output.unwrap_or_else(|| format!("{}.profile.yaml", target));
            std::fs::copy(&source, &out).map_err(|e| {
                AgentError::Io(format!("copy {} -> {}: {}", source.display(), out, e))
            })?;
            println!("Exported profile '{}' to {}", target, out);
        }
        "import" => {
            let source = name.ok_or_else(|| {
                AgentError::Config("profile import usage: hermes profile import <path>".into())
            })?;
            let source_path = PathBuf::from(&source);
            if !source_path.exists() {
                return Err(AgentError::Config(format!(
                    "profile import source not found: {}",
                    source_path.display()
                )));
            }
            let mut value = load_profile_yaml(&source_path)?;
            let target_name_raw = import_name.unwrap_or_else(|| {
                source_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            let target_name = validate_profile_name(&target_name_raw)?;
            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("mkdir {}: {}", profiles_dir.display(), e)))?;
            let target_path = profiles_dir.join(format!("{}.yaml", target_name));
            if target_path.exists() {
                let metadata = std::fs::metadata(&target_path).map_err(|e| {
                    AgentError::Io(format!("stat {}: {}", target_path.display(), e))
                })?;
                if metadata.is_dir() {
                    return Err(AgentError::Config(format!(
                        "Refusing to import profile: target path is a directory ({})",
                        target_path.display()
                    )));
                }
                if !yes {
                    return Err(AgentError::Config(format!(
                        "Target profile exists at {} (re-run with -y to overwrite)",
                        target_path.display()
                    )));
                }
            }
            if let Some(map) = value.as_mapping_mut() {
                map.insert(
                    serde_yaml::Value::String("name".to_string()),
                    serde_yaml::Value::String(target_name.clone()),
                );
            }
            let staged_path = profiles_dir.join(format!(
                ".{}.import-{}.yaml.tmp",
                target_name,
                uuid::Uuid::new_v4()
            ));
            save_profile_yaml(&staged_path, &value)?;
            if target_path.exists() {
                std::fs::remove_file(&target_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", target_path.display(), e))
                })?;
            }
            if let Err(err) = std::fs::rename(&staged_path, &target_path) {
                let _ = std::fs::remove_file(&staged_path);
                return Err(AgentError::Io(format!(
                    "rename {} -> {}: {}",
                    staged_path.display(),
                    target_path.display(),
                    err
                )));
            }
            if !no_alias {
                if let Some(alias) = alias_name
                    .or(secondary)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    aliases.insert(alias.clone(), target_name.clone());
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Added alias '{}' -> '{}'.", alias, target_name);
                }
            }
            println!(
                "Imported profile '{}' from {}",
                target_name,
                source_path.display()
            );
        }
        other => {
            println!(
                "Unknown profile action: '{}'. Use list|show|create|use|delete|alias|rename|export|import.",
                other
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::PlatformConfig;
    use hermes_config::session::SessionConfig;
    use hermes_gateway::dm::DmManager;
    use hermes_gateway::{Gateway, SessionManager};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    #[test]
    fn gateway_platform_menu_label_marks_configured_platforms() {
        let entry = &GATEWAY_PLATFORM_CATALOG[0];
        assert_eq!(entry.key, "telegram");
        let mut configured = make_platform(true, Some("tg-token"));
        configured.allowed_users = vec!["123456789".to_string()];
        let label = gateway_platform_menu_label(entry, Some(&configured));
        assert!(label.contains("Telegram"));
        assert!(label.contains("(configured)"));

        configured.allowed_users.clear();
        let label = gateway_platform_menu_label(entry, Some(&configured));
        assert!(label.contains("(not configured)"));

        configured.token = None;
        let label = gateway_platform_menu_label(entry, Some(&configured));
        assert!(label.contains("(not configured)"));
    }

    #[test]
    fn apply_telegram_allowlists_sets_policy_fields() {
        let mut platform = PlatformConfig::default();
        apply_telegram_allowlists(&mut platform, &["111".into(), "222".into()]);
        assert_eq!(platform.allowed_users, vec!["111", "222"]);
        assert_eq!(
            platform.extra.get("dm_policy").and_then(|v| v.as_str()),
            Some("allowlist")
        );
        let allow_from = platform
            .extra
            .get("allow_from")
            .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            .unwrap_or_default();
        assert_eq!(allow_from, vec!["111", "222"]);
    }

    fn cli_for_temp_state_root(temp_root: &std::path::Path) -> Cli {
        use clap::Parser;
        Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            temp_root.to_str().expect("utf8 path"),
        ])
    }

    fn make_platform(enabled: bool, token: Option<&str>) -> PlatformConfig {
        let mut cfg = PlatformConfig {
            enabled,
            ..Default::default()
        };
        if let Some(t) = token {
            cfg.token = Some(t.to_string());
        }
        cfg
    }

    fn make_gateway() -> Arc<Gateway> {
        Arc::new(Gateway::new(
            Arc::new(SessionManager::new(SessionConfig::default())),
            DmManager::with_pair_behavior(),
            hermes_gateway::gateway::GatewayConfig::default(),
        ))
    }

    #[test]
    fn gateway_agent_signature_changes_when_user_changes() {
        let cfg = GatewayConfig::default();
        let mut ctx_a = GatewayRuntimeContext::default();
        ctx_a.session_key = "wecom:room-1".to_string();
        ctx_a.platform = "wecom".to_string();
        ctx_a.user_id = "alice".to_string();
        let mut ctx_b = ctx_a.clone();
        ctx_b.user_id = "bob".to_string();
        assert_ne!(
            gateway_agent_signature(&cfg, &ctx_a),
            gateway_agent_signature(&cfg, &ctx_b)
        );
    }

    #[test]
    fn gateway_agent_signature_changes_when_personality_changes() {
        let cfg = GatewayConfig::default();
        let mut ctx_a = GatewayRuntimeContext::default();
        ctx_a.session_key = "wecom:room-1".to_string();
        ctx_a.platform = "wecom".to_string();
        ctx_a.user_id = "alice".to_string();
        ctx_a.personality = Some("default".to_string());
        let mut ctx_b = ctx_a.clone();
        ctx_b.personality = Some("strict".to_string());
        assert_ne!(
            gateway_agent_signature(&cfg, &ctx_a),
            gateway_agent_signature(&cfg, &ctx_b)
        );
    }

    #[tokio::test]
    async fn run_model_persists_default_model_to_config_yaml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        run_model(
            cli.clone(),
            Some("nous:nousresearch/hermes-4-70b".to_string()),
        )
        .await
        .expect("run model");

        let cfg = load_user_config_file(&tmp.path().join("config.yaml")).expect("load config");
        assert_eq!(cfg.model.as_deref(), Some("nous:nousresearch/hermes-4-70b"));
    }

    #[test]
    fn mask_secret_hides_token_body() {
        let raw = "abcdefgh1234567890";
        let masked = mask_secret(raw);
        assert!(!masked.contains(raw));
        assert!(masked.starts_with("abcd"));
        assert!(masked.ends_with("7890"));
        assert!(masked.contains("***"));
    }

    #[test]
    fn api_server_config_defaults_to_loopback() {
        let platform = PlatformConfig {
            enabled: true,
            ..Default::default()
        };
        let cfg = build_api_server_config(&platform);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8090);
        assert_eq!(cfg.auth_token, None);
    }

    #[test]
    fn api_server_config_honors_overrides_and_token_precedence() {
        let mut platform = PlatformConfig {
            enabled: true,
            token: Some("platform-token".to_string()),
            ..Default::default()
        };
        platform
            .extra
            .insert("host".to_string(), serde_json::json!("0.0.0.0"));
        platform
            .extra
            .insert("port".to_string(), serde_json::json!(9123));
        platform
            .extra
            .insert("auth_token".to_string(), serde_json::json!("extra-token"));

        let cfg = build_api_server_config(&platform);
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 9123);
        assert_eq!(cfg.auth_token.as_deref(), Some("platform-token"));
    }

    #[test]
    fn auth_provider_aliases_cover_primary_chains() {
        assert_eq!(normalize_auth_provider("tg"), "telegram");
        assert_eq!(normalize_auth_provider("wechat"), "weixin");
        assert_eq!(normalize_auth_provider("wx"), "weixin");
        assert_eq!(normalize_auth_provider("claude"), "anthropic");
        assert_eq!(normalize_auth_provider("codex"), "openai-codex");
        assert_eq!(normalize_auth_provider("openai-oauth"), "openai");
        assert_eq!(normalize_auth_provider("qwen-cli"), "qwen-oauth");
        assert_eq!(normalize_auth_provider("gemini-cli"), "google-gemini-cli");
        assert_eq!(normalize_auth_provider("step-plan"), "stepfun");
        assert_eq!(normalize_auth_provider("aigateway"), "ai-gateway");
        assert_eq!(normalize_auth_provider("moonshot"), "kimi-coding");
        assert_eq!(normalize_auth_provider("z-ai"), "zai");
        assert_eq!(normalize_auth_provider("grok"), "xai");
        assert_eq!(normalize_auth_provider("hf"), "huggingface");
        assert_eq!(normalize_auth_provider("ollama"), "ollama-local");
        assert_eq!(normalize_auth_provider("llama.cpp"), "llama-cpp");
        assert_eq!(normalize_auth_provider("ollvm"), "vllm");
        assert_eq!(normalize_auth_provider("llvm"), "vllm");
        assert_eq!(normalize_auth_provider("mlx-lm"), "mlx");
        assert_eq!(normalize_auth_provider("ane"), "apple-ane");
        assert_eq!(normalize_auth_provider("text-generation-inference"), "tgi");
        assert_eq!(normalize_auth_provider("api-server"), "api_server");
        assert_eq!(normalize_auth_provider("mm"), "mattermost");
    }

    #[test]
    fn oneshot_auto_verify_provider_detects_nous_401_errors() {
        let err = AgentError::LlmApi(
            "API error 401 Unauthorized: https://portal.nousresearch.com".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&err, Some("nous"), Some("nous:openai/gpt-5.5")),
            Some("nous".to_string())
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&err, None, Some("nous:moonshotai/kimi-k2.6")),
            Some("nous".to_string())
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&err, None, None),
            Some("nous".to_string())
        );
    }

    #[test]
    fn oneshot_auto_verify_provider_supports_core_oauth_providers() {
        let openai = AgentError::LlmApi("API error 401 Unauthorized: auth.openai.com".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&openai, Some("openai"), Some("openai:gpt-5.5")),
            Some("openai".to_string())
        );
        let codex = AgentError::LlmApi("API error 401 Unauthorized: chatgpt.com codex".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&codex, None, Some("openai-codex:codex-mini")),
            Some("openai-codex".to_string())
        );
        let anthropic = AgentError::LlmApi(
            "API error 401 Unauthorized: console.anthropic.com token expired".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&anthropic, Some("claude"), None),
            Some("anthropic".to_string())
        );
        let gemini = AgentError::LlmApi(
            "API error 401 Unauthorized: oauth2.googleapis.com invalid_grant".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&gemini, Some("gemini-cli"), None),
            Some("google-gemini-cli".to_string())
        );
        let qwen = AgentError::LlmApi(
            "API error 401 Unauthorized: chat.qwen.ai token expired".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&qwen, Some("qwen-cli"), None),
            Some("qwen-oauth".to_string())
        );
    }

    #[test]
    fn oneshot_auto_verify_provider_ignores_non_oauth_or_non_auth_errors() {
        let not_auth = AgentError::LlmApi("API error 404 Not Found".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(
                &not_auth,
                Some("nous"),
                Some("nous:openai/gpt-5.5")
            ),
            None
        );

        let other_provider = AgentError::LlmApi(
            "API error 401 Unauthorized: provider openrouter token expired".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(
                &other_provider,
                Some("openrouter"),
                Some("openrouter:openai/gpt-4o")
            ),
            None
        );

        let missing_signal = AgentError::LlmApi("API error 500 Internal Server Error".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(
                &missing_signal,
                Some("openai"),
                Some("openai:gpt-5.5")
            ),
            None
        );
    }

    #[test]
    fn oneshot_auth_is_refreshable_detects_auth_signals() {
        assert!(oneshot_auth_is_refreshable(
            "api error 401 unauthorized token expired"
        ));
        assert!(oneshot_auth_is_refreshable("invalid_grant"));
        assert!(!oneshot_auth_is_refreshable("api error 404 not found"));
    }

    #[test]
    fn infer_oauth_provider_from_error_message_maps_known_hosts() {
        assert_eq!(
            infer_oauth_provider_from_error_message("portal.nousresearch.com unauthorized"),
            Some("nous".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("auth.openai.com unauthorized"),
            Some("openai".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("chatgpt.com codex token expired"),
            Some("openai-codex".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("console.anthropic.com invalid token"),
            Some("anthropic".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("oauth2.googleapis.com invalid_grant"),
            Some("google-gemini-cli".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("chat.qwen.ai invalid token"),
            Some("qwen-oauth".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("openrouter.ai unauthorized"),
            None
        );
    }

    #[test]
    fn resolve_auth_type_prefers_oauth_for_supported_providers() {
        assert_eq!(resolve_auth_type_for_provider("nous", None), "oauth");
        assert_eq!(
            resolve_auth_type_for_provider("openai-codex", None),
            "oauth"
        );
        assert_eq!(resolve_auth_type_for_provider("qwen-oauth", None), "oauth");
        assert_eq!(
            resolve_auth_type_for_provider("google-gemini-cli", None),
            "oauth"
        );
        assert_eq!(resolve_auth_type_for_provider("anthropic", None), "oauth");
        assert_eq!(resolve_auth_type_for_provider("openai", None), "oauth");
        assert_eq!(
            resolve_auth_type_for_provider("openai", Some("API-KEY")),
            "api_key"
        );
        assert_eq!(
            resolve_auth_type_for_provider("openai", Some("oauth")),
            "oauth"
        );
    }

    #[test]
    fn oauth_refresh_config_defaults_cover_core_oauth_providers() {
        let _guard = env_lock();
        hermes_cli::env_vars::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL");
        hermes_cli::env_vars::remove_var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL");
        hermes_cli::env_vars::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID");
        hermes_cli::env_vars::remove_var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID");
        hermes_cli::env_vars::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
        hermes_cli::env_vars::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");

        let (openai_token_url, openai_client_id) =
            oauth_refresh_config_for_provider("openai").expect("openai config");
        assert_eq!(openai_token_url, CODEX_OAUTH_TOKEN_URL);
        assert_eq!(openai_client_id, CODEX_OAUTH_CLIENT_ID);

        let (codex_token_url, codex_client_id) =
            oauth_refresh_config_for_provider("openai-codex").expect("codex config");
        assert_eq!(codex_token_url, CODEX_OAUTH_TOKEN_URL);
        assert_eq!(codex_client_id, CODEX_OAUTH_CLIENT_ID);

        let (anthropic_token_url, anthropic_client_id) =
            oauth_refresh_config_for_provider("anthropic").expect("anthropic config");
        assert_eq!(anthropic_token_url, ANTHROPIC_OAUTH_TOKEN_URL);
        assert_eq!(anthropic_client_id, ANTHROPIC_OAUTH_CLIENT_ID);

        assert!(oauth_refresh_config_for_provider("nous").is_none());
    }

    #[test]
    fn auth_verify_source_priority_is_env_then_store_then_state() {
        assert_eq!(auth_verify_source(true, true, true), "env");
        assert_eq!(auth_verify_source(false, true, true), "token_store");
        assert_eq!(auth_verify_source(false, false, true), "auth_json");
        assert_eq!(auth_verify_source(false, false, false), "none");
    }

    #[test]
    fn provider_env_var_maps_stepfun() {
        assert_eq!(provider_env_var("stepfun"), Some("STEPFUN_API_KEY"));
        assert_eq!(provider_env_var("step"), None);
        assert_eq!(
            provider_env_var("openai-codex"),
            Some("HERMES_OPENAI_CODEX_API_KEY")
        );
        assert_eq!(
            provider_env_var("qwen-oauth"),
            Some("HERMES_QWEN_OAUTH_API_KEY")
        );
        assert_eq!(
            provider_env_var("google-gemini-cli"),
            Some("HERMES_GEMINI_OAUTH_API_KEY")
        );
        assert_eq!(secret_provider_aliases("stepfun"), vec!["stepfun", "step"]);
        assert_eq!(
            secret_provider_aliases("claude"),
            vec!["anthropic", "claude", "claude-code"]
        );
        assert_eq!(provider_env_var("ollama"), Some("OLLAMA_LOCAL_API_KEY"));
        assert_eq!(provider_env_var("llama.cpp"), Some("LLAMA_CPP_API_KEY"));
        assert_eq!(provider_env_var("ollvm"), Some("VLLM_API_KEY"));
        assert_eq!(provider_env_var("mlx-lm"), Some("MLX_API_KEY"));
        assert_eq!(provider_env_var("ane"), Some("APPLE_ANE_API_KEY"));
        assert_eq!(
            provider_env_var("text-generation-inference"),
            Some("TGI_API_KEY")
        );
    }

    #[test]
    fn matrix_home_room_prefers_platform_config_then_env_fallback() {
        let _guard = env_lock();
        let previous = std::env::var("MATRIX_HOME_ROOM").ok();

        let mut platform = PlatformConfig::default();
        platform
            .extra
            .insert("room_id".to_string(), serde_json::json!("!cfg:matrix.org"));
        hermes_cli::env_vars::set_var("MATRIX_HOME_ROOM", "!env:matrix.org");
        assert_eq!(
            matrix_home_room_for_platform(&platform).as_deref(),
            Some("!cfg:matrix.org")
        );

        platform.extra.remove("room_id");
        assert_eq!(
            matrix_home_room_for_platform(&platform).as_deref(),
            Some("!env:matrix.org")
        );

        match previous {
            Some(value) => hermes_cli::env_vars::set_var("MATRIX_HOME_ROOM", value),
            None => hermes_cli::env_vars::remove_var("MATRIX_HOME_ROOM"),
        }
    }

    #[test]
    fn setup_model_choice_supports_nous() {
        let option = &SETUP_MODEL_OPTIONS[default_setup_model_choice().saturating_sub(1)];
        assert_eq!(option.model, "nous:openai/gpt-5.5-pro");
        assert_eq!(option.provider, "nous");
    }

    #[test]
    fn setup_provider_defaults_are_unique_and_include_nous() {
        let providers = setup_provider_defaults();
        assert!(!providers.is_empty());
        let mut seen = std::collections::BTreeSet::new();
        for option in providers {
            assert!(
                seen.insert(option.provider),
                "duplicate provider {}",
                option.provider
            );
        }
        assert!(seen.contains("nous"));
    }

    #[test]
    fn setup_default_model_pick_index_matches_provider_prefixed_target() {
        let suggested = vec![
            "nousresearch/hermes-3-llama-3.1-405b".to_string(),
            "openai/gpt-5.5-pro".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let idx = setup_default_model_pick_index("nous", "nous:openai/gpt-5.5-pro", &suggested);
        assert_eq!(idx, 1);
    }

    #[test]
    fn setup_default_model_pick_index_uses_nous_kimi_fallback_when_target_missing() {
        let suggested = vec![
            "nousresearch/hermes-3-llama-3.1-405b".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
            "openai/gpt-5.5".to_string(),
        ];
        let idx = setup_default_model_pick_index("nous", "nous:nonexistent/model", &suggested);
        assert_eq!(idx, 1);
    }

    #[test]
    fn setup_default_model_pick_index_falls_back_to_zero_for_non_nous() {
        let suggested = vec![
            "gpt-4o".to_string(),
            "gpt-4o-mini".to_string(),
            "gpt-5.4".to_string(),
        ];
        let idx = setup_default_model_pick_index("openai", "openai:not-real", &suggested);
        assert_eq!(idx, 0);
    }

    #[test]
    fn setup_provider_env_keys_include_nous() {
        assert_eq!(setup_provider_display("nous"), "Nous");
        assert_eq!(setup_provider_env_keys("nous"), &["NOUS_API_KEY"]);
        assert_eq!(
            setup_provider_env_keys("ollama-local"),
            &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("vllm"),
            Some("http://127.0.0.1:8000/v1")
        );
        assert!(!setup_provider_requires_api_key("ollama-local"));
        assert!(!setup_provider_requires_api_key("apple-ane"));
        assert!(setup_provider_requires_api_key("openai"));
        assert_eq!(setup_provider_display("alibaba"), "Alibaba Cloud DashScope");
        assert_eq!(
            setup_provider_env_keys("google-gemini-cli"),
            &["HERMES_GEMINI_OAUTH_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("ai-gateway"),
            Some("https://ai-gateway.vercel.sh/v1")
        );
        assert!(
            SETUP_MODEL_OPTIONS.len() >= 20,
            "setup provider catalog unexpectedly narrow"
        );
    }

    #[test]
    fn oauth_provider_set_matches_snapshot_registry() {
        let actual: std::collections::BTreeSet<&str> =
            hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
                .iter()
                .copied()
                .collect();
        let expected_minimum: std::collections::BTreeSet<&str> = [
            "anthropic",
            "nous",
            "openai-codex",
            "qwen-oauth",
            "google-gemini-cli",
        ]
        .into_iter()
        .collect();
        let missing: Vec<&str> = expected_minimum
            .iter()
            .copied()
            .filter(|provider| !actual.contains(provider))
            .collect();
        assert!(
            missing.is_empty(),
            "missing upstream oauth providers: {:?}",
            missing
        );
        assert!(
            actual.contains("openai"),
            "OpenAI OAuth should be enabled in Hermes Ultra"
        );
    }

    #[tokio::test]
    async fn hydrate_provider_env_from_vault_overrides_oauth_provider_env() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let vault_path = secret_vault_path_for_cli(&cli);
        let store = FileTokenStore::new(vault_path).await.expect("vault store");
        let manager = AuthManager::new(store);
        manager
            .save_credential(OAuthCredential {
                provider: "nous".to_string(),
                access_token: "vault-good-key".to_string(),
                refresh_token: None,
                token_type: "bearer".to_string(),
                scope: None,
                expires_at: None,
            })
            .await
            .expect("save vault credential");

        let previous = std::env::var("NOUS_API_KEY").ok();
        hermes_cli::env_vars::set_var("NOUS_API_KEY", "env-stale-key");

        hydrate_provider_env_from_vault_for_cli(&cli)
            .await
            .expect("hydrate env");
        assert_eq!(
            std::env::var("NOUS_API_KEY").as_deref(),
            Ok("vault-good-key")
        );

        match previous {
            Some(value) => hermes_cli::env_vars::set_var("NOUS_API_KEY", value),
            None => hermes_cli::env_vars::remove_var("NOUS_API_KEY"),
        }
    }

    #[test]
    fn read_env_key_treats_empty_values_as_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        std::fs::write(
            &env_file,
            "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='   '\nOPENAI_API_KEY=real-key\n",
        )
        .expect("write env");

        assert_eq!(read_env_key(&env_file, "OPENROUTER_API_KEY"), None);
        assert_eq!(read_env_key(&env_file, "MINIMAX_API_KEY"), None);
        assert_eq!(
            read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
            Some("real-key")
        );
    }

    #[test]
    fn merge_missing_env_keys_skips_empty_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("legacy.env");
        let dst = tmp.path().join("target.env");
        std::fs::write(
            &src,
            "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='  '\nOPENAI_API_KEY=real-key\n",
        )
        .expect("write source env");

        let imported = merge_missing_env_keys(&src, &dst, "legacy.env").expect("merge env keys");
        assert_eq!(imported, 1);
        let contents = std::fs::read_to_string(&dst).expect("read merged env");
        assert!(contents.contains("OPENAI_API_KEY=real-key"));
        assert!(!contents.contains("OPENROUTER_API_KEY="));
        assert!(!contents.contains("MINIMAX_API_KEY="));
    }

    #[test]
    fn read_env_key_handles_non_utf8_bytes_without_crashing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        let mut bytes = b"OPENAI_API_KEY=real-key\nBROKEN=".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, 0x81, b'\n']);
        std::fs::write(&env_file, bytes).expect("write non-utf8 env");

        assert_eq!(
            read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
            Some("real-key")
        );
    }

    #[test]
    fn upsert_env_key_rewrites_existing_and_appends_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        std::fs::write(
            &env_file,
            "OPENAI_API_KEY=old\nHERMES_AUTH_DEFAULT_PROVIDER=openai\n",
        )
        .expect("write env");
        upsert_env_key(&env_file, "HERMES_AUTH_DEFAULT_PROVIDER", "nous").expect("upsert");
        upsert_env_key(&env_file, "NOUS_API_KEY", "tok").expect("append");
        let raw = std::fs::read_to_string(&env_file).expect("read env");
        assert!(raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=nous"));
        assert!(raw.contains("NOUS_API_KEY=tok"));
        assert!(!raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=openai"));
    }

    #[tokio::test]
    async fn profile_create_no_skills_strips_cloned_skill_overrides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

        let source_profile = profiles_dir.join("source.yaml");
        std::fs::write(
            &source_profile,
            r#"
name: source
model: openai:gpt-4o
personality: technical
max_turns: 50
skills:
  enabled:
    - contextlattice-agent-contract
  disabled:
    - noisy-skill
"#,
        )
        .expect("write source profile");
        write_active_profile_name(&profiles_dir, "source").expect("set active profile");

        run_profile(
            cli,
            Some("create".to_string()),
            Some("target".to_string()),
            None,
            None,
            None,
            None,
            false,
            false,
            true,
            true,
            Some("source".to_string()),
            true,
            true,
        )
        .await
        .expect("create profile");

        let target_profile = profiles_dir.join("target.yaml");
        let parsed: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(&target_profile).expect("read target profile"),
        )
        .expect("parse target profile");
        let map = parsed.as_mapping().expect("mapping profile");
        let skills_key = serde_yaml::Value::String("skills".to_string());
        assert!(
            !map.contains_key(&skills_key),
            "skills key should be stripped"
        );
    }

    #[test]
    fn validate_profile_name_rejects_paths() {
        let err = validate_profile_name("../danger").expect_err("should reject traversal");
        assert!(
            err.to_string().contains("path separators"),
            "unexpected error: {err}"
        );
        let err = validate_profile_name("alpha beta").expect_err("should reject spaces");
        assert!(
            err.to_string().contains("letters, numbers"),
            "unexpected error: {err}"
        );
        assert_eq!(
            validate_profile_name("prod-profile_1.2").expect("valid"),
            "prod-profile_1.2"
        );
    }

    #[tokio::test]
    async fn profile_import_refuses_directory_clobber_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

        let source_profile = tmp.path().join("source.yaml");
        std::fs::write(
            &source_profile,
            r#"
name: source
model: openai:gpt-4o
personality: default
max_turns: 50
"#,
        )
        .expect("write source profile");

        let clobber_target_dir = profiles_dir.join("target.yaml");
        std::fs::create_dir_all(&clobber_target_dir).expect("create clobber directory");

        let err = run_profile(
            cli,
            Some("import".to_string()),
            Some(source_profile.to_string_lossy().into_owned()),
            None,
            None,
            Some("target".to_string()),
            None,
            false,
            true,
            false,
            false,
            None,
            true,
            false,
        )
        .await
        .expect_err("directory clobber should be rejected");

        assert!(
            err.to_string().contains("target path is a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn wecom_qr_page_url_encodes_scode() {
        let url = wecom_qr_page_url("abc/def");
        assert!(url.contains("abc%2Fdef"));
        assert!(url.starts_with("https://work.weixin.qq.com/ai/qc/gen?source=hermes&scode="));
    }

    #[test]
    fn qqbot_connect_url_encodes_task_id() {
        let url = qqbot_connect_url("task id/+");
        assert!(url.contains("task_id=task%20id%2F%2B"));
        assert!(url.contains("source=hermes"));
    }

    #[test]
    fn qqbot_decrypt_secret_roundtrip() {
        let key = [7u8; 32];
        let nonce = [3u8; 12];
        let key_b64 = BASE64_STANDARD.encode(key);

        let cipher =
            <Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key).expect("cipher init");
        let ciphertext = cipher
            .encrypt(aes_gcm::Nonce::from_slice(&nonce), b"qq-secret".as_ref())
            .expect("encrypt");
        let mut payload = nonce.to_vec();
        payload.extend_from_slice(&ciphertext);
        let encrypted_b64 = BASE64_STANDARD.encode(payload);

        let decrypted = qqbot_decrypt_secret(&encrypted_b64, &key_b64).expect("decrypt");
        assert_eq!(decrypted, "qq-secret");
    }

    #[test]
    fn qqbot_extract_i64_accepts_number_or_string() {
        let numeric = serde_json::json!({ "status": 2 });
        assert_eq!(qqbot_extract_i64(&numeric, &["status"]), Some(2));

        let stringified = serde_json::json!({ "status": "3" });
        assert_eq!(qqbot_extract_i64(&stringified, &["status"]), Some(3));
    }

    #[test]
    fn query_is_local_slash_command_detects_prefixed_queries() {
        assert!(query_is_local_slash_command("/model list"));
        assert!(query_is_local_slash_command("   /graph status"));
        assert!(!query_is_local_slash_command("hello world"));
    }

    #[test]
    fn capture_debug_log_snapshot_preserves_boundary_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("hermes.log");
        std::fs::write(&log_path, "line1\nline2\nline3\n").expect("write log");

        let snap = capture_debug_log_snapshot(&log_path, 1, 12);
        let full = snap.full_text.unwrap_or_default();
        assert!(full.contains("line2\nline3"));
        assert!(!full.contains("line1"));
    }

    #[test]
    fn capture_debug_log_snapshot_caps_memory_with_long_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("hermes.log");
        let long = "x".repeat(256 * 1024);
        std::fs::write(&log_path, long).expect("write long log");

        let max_bytes = 4096usize;
        let snap = capture_debug_log_snapshot(&log_path, 5, max_bytes);
        let full = snap.full_text.unwrap_or_default();
        assert!(
            full.len() <= (max_bytes * 2) + 128,
            "full snapshot should obey hard cap"
        );
    }

    #[test]
    fn capture_debug_log_snapshot_distinguishes_missing_and_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing.log");
        let missing_snap = capture_debug_log_snapshot(&missing, 10, 1024);
        assert_eq!(missing_snap.tail_text, "(file not found)");

        let empty = tmp.path().join("empty.log");
        std::fs::write(&empty, "").expect("write empty log");
        let empty_snap = capture_debug_log_snapshot(&empty, 10, 1024);
        assert_eq!(empty_snap.tail_text, "(file empty)");
    }

    #[test]
    fn sweep_expired_pending_pastes_is_best_effort_and_keeps_fresh_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reports_dir = tmp.path();
        let store = debug_pending_pastes_path(reports_dir);
        let entries = vec![
            PendingPasteDelete {
                url: "https://paste.rs/expired".to_string(),
                expires_at_unix: 100,
            },
            PendingPasteDelete {
                url: "https://paste.rs/fresh".to_string(),
                expires_at_unix: 9_999_999_999,
            },
        ];
        std::fs::write(
            &store,
            serde_json::to_string_pretty(&entries).expect("serialize"),
        )
        .expect("write pending store");

        let removed = sweep_expired_pending_pastes(reports_dir, 1_000).expect("sweep");
        assert_eq!(removed, 1);

        let kept: Vec<PendingPasteDelete> =
            serde_json::from_str(&std::fs::read_to_string(&store).expect("read pending store"))
                .expect("parse pending store");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].url, "https://paste.rs/fresh");
    }

    #[test]
    fn best_effort_sweep_handles_invalid_store_without_failing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reports_dir = tmp.path();
        let store = debug_pending_pastes_path(reports_dir);
        std::fs::write(&store, "{invalid json").expect("write invalid json");

        let removed = best_effort_sweep_expired_pending_pastes(reports_dir, 1_000);
        assert_eq!(removed, 0);
    }

    #[test]
    fn run_sessions_db_auto_maintenance_degrades_when_home_is_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bad_home = tmp.path().join("not-a-dir");
        std::fs::write(&bad_home, "x").expect("write blocker file");

        let mut cfg = hermes_config::GatewayConfig::default();
        cfg.home_dir = Some(bad_home.to_string_lossy().to_string());
        cfg.sessions.auto_prune = true;

        let result = std::panic::catch_unwind(|| run_sessions_db_auto_maintenance(&cfg));
        assert!(
            result.is_ok(),
            "maintenance should degrade without panicking"
        );
    }

    #[test]
    fn gateway_auth_provider_keys_include_primary_platforms() {
        for key in ["telegram", "weixin", "discord", "slack"] {
            let mapped = gateway_platform_provider_key(key);
            if key == "telegram" || key == "weixin" {
                assert!(mapped.is_none(), "{key} handled by dedicated auth flow");
            } else {
                assert_eq!(mapped, Some(key));
            }
        }
    }

    #[test]
    fn gateway_requirement_check_flags_missing_required_fields() {
        let mut config = hermes_config::GatewayConfig::default();
        config
            .platforms
            .insert("telegram".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("qqbot".to_string(), make_platform(true, None));
        let issues = gateway_requirement_issues(&config);
        assert!(issues.iter().any(|s| s.contains("telegram")));
        assert!(issues.iter().any(|s| s.contains("qqbot")));
    }

    #[test]
    fn gateway_requirement_check_accepts_complete_qqbot_and_wecom_callback() {
        let mut config = hermes_config::GatewayConfig::default();

        let mut qqbot = make_platform(true, None);
        qqbot
            .extra
            .insert("app_id".to_string(), serde_json::json!("qq-app"));
        qqbot
            .extra
            .insert("client_secret".to_string(), serde_json::json!("qq-secret"));
        config.platforms.insert("qqbot".to_string(), qqbot);

        let mut wecom_cb = make_platform(true, Some("cb-token"));
        wecom_cb
            .extra
            .insert("corp_id".to_string(), serde_json::json!("wwcorp"));
        wecom_cb
            .extra
            .insert("corp_secret".to_string(), serde_json::json!("corp-secret"));
        wecom_cb
            .extra
            .insert("agent_id".to_string(), serde_json::json!("1000002"));
        wecom_cb.extra.insert(
            "encoding_aes_key".to_string(),
            serde_json::json!("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
        );
        config
            .platforms
            .insert("wecom_callback".to_string(), wecom_cb);

        assert!(gateway_requirement_issues(&config).is_empty());
    }

    #[tokio::test]
    async fn register_gateway_adapters_registers_primary_platforms_when_config_is_complete() {
        let mut config = hermes_config::GatewayConfig::default();

        let mut telegram = make_platform(true, Some("tg-token"));
        telegram
            .extra
            .insert("polling".to_string(), serde_json::json!(false));
        config.platforms.insert("telegram".to_string(), telegram);

        let mut weixin = make_platform(true, Some("wx-token"));
        weixin
            .extra
            .insert("account_id".to_string(), serde_json::json!("wxid_abc"));
        config.platforms.insert("weixin".to_string(), weixin);

        config.platforms.insert(
            "discord".to_string(),
            make_platform(true, Some("discord-token")),
        );
        config
            .platforms
            .insert("slack".to_string(), make_platform(true, Some("xoxb-slack")));

        let gateway = make_gateway();
        let mut sidecar_tasks = Vec::new();
        register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
            .await
            .expect("primary platform registration should succeed");

        let mut names = gateway.adapter_names().await;
        names.sort();
        assert!(names.contains(&"telegram".to_string()));
        assert!(names.contains(&"weixin".to_string()));
        assert!(names.contains(&"discord".to_string()));
        assert!(names.contains(&"slack".to_string()));

        for task in sidecar_tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn register_gateway_adapters_skips_primary_platforms_when_required_credentials_missing() {
        let mut config = hermes_config::GatewayConfig::default();
        config
            .platforms
            .insert("telegram".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("weixin".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("discord".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("slack".to_string(), make_platform(true, None));

        let gateway = make_gateway();
        let mut sidecar_tasks = Vec::new();
        register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
            .await
            .expect("missing credentials should be handled gracefully");

        assert!(
            gateway.adapter_names().await.is_empty(),
            "no primary adapter should register when required credentials are missing"
        );
        for task in sidecar_tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn register_gateway_adapters_registers_qqbot_and_wecom_callback() {
        let mut config = hermes_config::GatewayConfig::default();

        let mut qqbot = make_platform(true, None);
        qqbot
            .extra
            .insert("app_id".to_string(), serde_json::json!("qq-app"));
        qqbot
            .extra
            .insert("client_secret".to_string(), serde_json::json!("qq-secret"));
        config.platforms.insert("qqbot".to_string(), qqbot);

        let mut wecom_cb = make_platform(true, None);
        wecom_cb
            .extra
            .insert("corp_id".to_string(), serde_json::json!("wwcorp"));
        wecom_cb
            .extra
            .insert("corp_secret".to_string(), serde_json::json!("corp-secret"));
        wecom_cb
            .extra
            .insert("agent_id".to_string(), serde_json::json!("1000002"));
        wecom_cb
            .extra
            .insert("token".to_string(), serde_json::json!("cb-token"));
        wecom_cb.extra.insert(
            "encoding_aes_key".to_string(),
            serde_json::json!("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
        );
        config
            .platforms
            .insert("wecom_callback".to_string(), wecom_cb);

        let gateway = make_gateway();
        let mut sidecar_tasks = Vec::new();
        register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
            .await
            .expect("qqbot and wecom_callback should register");

        let names = gateway.adapter_names().await;
        assert!(names.contains(&"qqbot".to_string()));
        assert!(names.contains(&"wecom_callback".to_string()));
    }

    #[test]
    fn doctor_self_heal_creates_missing_state_dirs() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "doctor",
        ]);
        let state_root = hermes_state_root(&cli);
        assert!(!state_root.join("profiles").exists());

        let actions = run_doctor_self_heal(&cli);
        assert!(state_root.join("profiles").exists());
        assert!(state_root.join("sessions").exists());
        assert!(state_root.join("logs").exists());
        assert!(
            actions
                .iter()
                .any(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("created"))
        );
    }

    #[test]
    fn doctor_self_heal_removes_stale_gateway_pid_file() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "doctor",
        ]);
        let pid_path = gateway_pid_path_for_cli(&hermes_state_root(&cli));
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir pid dir");
        }
        std::fs::write(&pid_path, "999999").expect("write stale pid");
        assert!(pid_path.exists());

        let actions = run_doctor_self_heal(&cli);
        assert!(!pid_path.exists(), "stale pid file should be removed");
        assert!(actions.iter().any(|entry| {
            entry
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("removed stale gateway pid file")
        }));
    }

    #[test]
    fn doctor_elite_diagnostics_payload_has_required_sections() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "doctor",
        ]);
        let payload = build_elite_doctor_diagnostics(&cli);
        assert!(payload.get("provenance").is_some());
        assert!(payload.get("route_learning").is_some());
        assert!(payload.get("route_health").is_some());
        assert!(payload.get("tool_policy").is_some());
        assert!(payload.get("elite_gate").is_some());
    }

    #[test]
    fn replay_integrity_detects_chain_break() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let replay = tmp.path().join("session.jsonl");
        std::fs::write(
            &replay,
            r#"{"seq":1,"event":"a","prev_hash":"seed","event_hash":"h1","payload":{"ok":true}}
{"seq":2,"event":"b","prev_hash":"BROKEN","event_hash":"h2","payload":{"ok":true}}
"#,
        )
        .expect("write replay");

        let summary = replay_integrity_for_file(&replay);
        assert_eq!(summary.events, 2);
        assert!(!summary.hash_chain_ok);
    }

    #[test]
    fn replay_manifest_aggregates_counts() {
        let items = vec![
            ReplayIntegritySummary {
                file: "a.jsonl".to_string(),
                checksum_sha256: Some("abc".to_string()),
                events: 3,
                invalid_lines: 0,
                hash_chain_ok: true,
                last_event_hash: Some("h1".to_string()),
            },
            ReplayIntegritySummary {
                file: "b.jsonl".to_string(),
                checksum_sha256: Some("def".to_string()),
                events: 2,
                invalid_lines: 1,
                hash_chain_ok: false,
                last_event_hash: Some("h2".to_string()),
            },
        ];
        let manifest = replay_manifest_json(&items);
        assert_eq!(manifest["totals"]["files"], 2);
        assert_eq!(manifest["totals"]["events"], 5);
        assert_eq!(manifest["totals"]["invalid_lines"], 1);
        assert_eq!(manifest["totals"]["hash_chain_ok"], false);
    }
}
