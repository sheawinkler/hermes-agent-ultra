//! Hermes Agent — binary entry point.
//!
//! Initializes logging, parses CLI arguments, and dispatches to the
//! appropriate subcommand handler.

use aes_gcm::aead::Aead;
use aes_gcm::Aes256Gcm;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::{generate, Shell as CompletionShell};
use hermes_agent::provider_profiles;
use hermes_agent::session_persistence::SessionPersistence;
use hermes_agent::{leading_system_prompt_for_persist, AgentCallbacks, AgentLoop};
use hermes_auth::{
    exchange_refresh_token, AuthManager, FileTokenStore, OAuth2Endpoints, OAuthCredential,
};
use hermes_cli::app::{
    bridge_tool_registry, build_agent_config, build_provider, provider_api_key_from_env,
};
use hermes_cli::auth::{
    clear_provider_auth_state, discover_existing_anthropic_oauth, discover_existing_nous_oauth,
    discover_existing_openai_codex_oauth, discover_existing_openai_oauth,
    get_anthropic_oauth_status, get_gemini_oauth_auth_status, get_qwen_auth_status,
    login_anthropic_oauth, login_google_gemini_cli_oauth, login_nous_device_code,
    login_openai_codex_device_code, login_openai_device_code, read_provider_auth_state,
    resolve_gemini_oauth_runtime_credentials, resolve_nous_runtime_credentials,
    resolve_qwen_runtime_credentials, save_codex_auth_state, save_nous_auth_state,
    save_openai_auth_state, save_provider_auth_state, AnthropicOAuthLoginOptions,
    CodexDeviceCodeOptions, GeminiOAuthLoginOptions, NousAuthState, NousDeviceCodeOptions,
    NousRuntimeCredentials, ANTHROPIC_OAUTH_CLIENT_ID, ANTHROPIC_OAUTH_TOKEN_URL,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL, DEFAULT_CODEX_BASE_URL,
    DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS, DEFAULT_NOUS_CLIENT_ID, DEFAULT_NOUS_INFERENCE_URL,
    DEFAULT_NOUS_PORTAL_URL, DEFAULT_OPENAI_BASE_URL, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, QWEN_OAUTH_CLIENT_ID, QWEN_OAUTH_TOKEN_URL,
};
use hermes_cli::cli::{Cli, CliCommand};
use hermes_cli::config_env::hydrate_env_from_config;
use hermes_cli::model_switch::{
    cached_provider_catalog_status, curated_provider_slugs, normalize_provider_model,
    provider_catalog_entries, provider_model_ids, provider_picker_description,
};
use hermes_cli::platform_toolsets::{resolve_platform_tool_schemas, tool_definition_summary};
use hermes_cli::providers::provider_capability_for;
use hermes_cli::runtime_tool_wiring::{
    wire_cron_scheduler_backend, wire_gateway_clarify_backend, wire_gateway_messaging_backend,
};
use hermes_cli::terminal_backend::build_terminal_backend;
use hermes_cli::tool_preview::{build_tool_preview_from_value, tool_emoji};
use hermes_cli::App;
use hermes_config::{
    gateway_pid_path_in, hermes_home, load_config, load_user_config_file, save_config_yaml,
    set_user_config_value, state_dir, user_config_field_display, validate_config, ConfigError,
    GatewayConfig, PlatformConfig, UnauthorizedDmBehavior,
};
use hermes_core::AgentError;
use hermes_core::PlatformAdapter;
use hermes_core::{MessageRole, StreamChunk};
use hermes_cron::{
    cron_scheduler_for_data_dir, CronCompletionEvent, CronError, CronRunner, CronScheduler,
    FileJobPersistence,
};
use hermes_gateway::gateway::GatewayConfig as RuntimeGatewayConfig;
use hermes_gateway::gateway::IncomingMessage as GatewayIncomingMessage;
use hermes_gateway::gateway::{GroupAccessMode, PlatformAccessPolicy};
use hermes_gateway::hooks::HookRegistry;
use hermes_gateway::platforms::api_server::{ApiInboundRequest, ApiServerAdapter, ApiServerConfig};
use hermes_gateway::platforms::bluebubbles::{BlueBubblesAdapter, BlueBubblesConfig};
use hermes_gateway::platforms::dingtalk::{DingTalkAdapter, DingTalkConfig};
use hermes_gateway::platforms::discord::{
    DiscordAdapter, DiscordChannelControls, DiscordChannelSkillBinding, DiscordConfig,
};
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
use hermes_gateway::platforms::telegram::{
    IncomingMessage as TelegramIncomingMessage, TelegramAdapter, TelegramConfig,
    TelegramTextBatcher,
};
use hermes_gateway::platforms::webhook::{WebhookAdapter, WebhookConfig, WebhookPayload};
use hermes_gateway::platforms::wecom::{WeComAdapter, WeComConfig};
use hermes_gateway::platforms::wecom_callback::{
    WeComCallbackAdapter, WeComCallbackApp, WeComCallbackConfig,
};
use hermes_gateway::platforms::weixin::{WeChatAdapter, WeixinConfig};
use hermes_gateway::platforms::whatsapp::{WhatsAppAdapter, WhatsAppConfig};
use hermes_gateway::tool_backends::ClarifyDispatcher;
use hermes_gateway::{DmManager, Gateway, GatewayRuntimeContext, SessionManager};
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_telemetry::init_telemetry_from_env;
use hermes_tools::{default_tool_policy_counters_path, load_tool_policy_counters, ToolRegistry};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Some(config_dir) = cli.config_dir.as_deref() {
        std::env::set_var("HERMES_HOME", config_dir);
    }
    if cli.ignore_user_config {
        std::env::set_var("HERMES_IGNORE_USER_CONFIG", "1");
    }
    if cli.ignore_rules {
        std::env::set_var("HERMES_IGNORE_RULES", "1");
        std::env::set_var("HERMES_AGENT_SKIP_CONTEXT_FILES", "1");
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
    );
    if let Err(err) = hydrate_provider_env_from_vault_for_cli(&cli).await {
        tracing::warn!("Secret-vault hydration skipped: {}", err);
    }
    if let Ok(cfg) = load_config(cli.config_dir.as_deref()) {
        let applied = hydrate_env_from_config(&cfg);
        tracing::debug!(
            applied_env_vars = applied,
            "Hydrated environment from config.yaml"
        );
    }
    let route_autotune_applied = apply_route_autotune_env_overrides(&cli);
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
            if let Some(prompt) = query.clone() {
                match handle_local_slash_query(cli.clone(), &prompt).await {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        hermes_cli::commands::handle_cli_chat(
                            query,
                            preload_skill,
                            yolo,
                            global_model_override.clone(),
                            global_provider_override.clone(),
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
                    global_model_override.clone(),
                    global_provider_override.clone(),
                    global_allow_tools_override,
                )
                .await
            }
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
        CliCommand::Update { check } => run_update(check).await,
        CliCommand::EliteCheck { json, strict } => run_elite_check(cli, json, strict).await,
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
            remove,
            yes,
            sync,
        } => hermes_cli::commands::handle_cli_skills(action, name, extra, remove, yes, sync).await,
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
        CliCommand::Pairing { action, device_id } => {
            hermes_cli::commands::handle_cli_pairing(action, device_id).await
        }
        CliCommand::Claw { action } => hermes_cli::commands::handle_cli_claw(action).await,
        CliCommand::Acp {
            action,
            check,
            setup,
            setup_browser,
            version,
            yes,
        } => {
            let action = acp_action_from_flags(action, check, setup, setup_browser, version);
            if action.as_deref() == Some("setup") {
                let mut result = run_model(cli, None).await;
                if result.is_ok() {
                    if yes {
                        result = hermes_cli::commands::handle_cli_acp(
                            Some("setup-browser".to_string()),
                            true,
                        )
                        .await;
                    } else if std::io::stdin().is_terminal() {
                        print!("Set up ACP browser tools now? [y/N] ");
                        let _ = std::io::stdout().flush();
                        let mut answer = String::new();
                        if std::io::stdin().read_line(&mut answer).is_ok()
                            && acp_setup_browser_answer_is_yes(&answer)
                        {
                            result = hermes_cli::commands::handle_cli_acp(
                                Some("setup-browser".to_string()),
                                false,
                            )
                            .await;
                        }
                    }
                }
                result
            } else {
                hermes_cli::commands::handle_cli_acp(action, yes).await
            }
        }
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
            workdir,
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
                workdir,
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
        CliCommand::Dump { session, output } => run_dump(cli, session, output).await,
        CliCommand::Completion { shell } => run_completion(shell),
        CliCommand::Uninstall { yes } => run_uninstall(yes).await,
        CliCommand::Lumio { action, model } => run_lumio(action, model).await,
        CliCommand::PluginExternal(raw) => {
            hermes_cli::commands::handle_cli_external_plugin_subcommand(raw).await
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Initialize the tracing subscriber with env filter.
fn init_tracing(verbose: bool, interactive_tui: bool) {
    let default = if interactive_tui {
        if verbose {
            "info"
        } else {
            "error"
        }
    } else if verbose {
        "debug"
    } else {
        "warn"
    };
    if interactive_tui
        && std::env::var("HERMES_TUI_ALLOW_STDERR_LOGS")
            .ok()
            .as_deref()
            != Some("1")
    {
        std::env::set_var("RUST_LOG", default);
    }
    init_telemetry_from_env("hermes-cli", default);
}

const INTERACTIVE_SESSION_LOCK_FILE: &str = "interactive.session.lock";
const INTERACTIVE_SESSION_LOCK_BYPASS_ENV: &str = "HERMES_ALLOW_PARALLEL_INTERACTIVE";

fn interactive_tty_error_message() -> &'static str {
    "interactive Hermes requires a terminal (TTY). Run `hermes-ultra setup` first, \
     use `hermes-ultra chat --query \"...\"` for non-interactive prompts, or run \
     `hermes-ultra doctor --deep --snapshot --bundle` for diagnostics."
}

fn require_interactive_tty() -> Result<(), AgentError> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Ok(())
    } else {
        Err(AgentError::Config(interactive_tty_error_message().into()))
    }
}

fn interactive_lock_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join(INTERACTIVE_SESSION_LOCK_FILE)
}

fn read_interactive_lock_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(pid) = trimmed.parse::<u32>() {
        return Some(pid);
    }
    let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pid = json.get("pid")?.as_u64()?;
    u32::try_from(pid).ok()
}

#[cfg(unix)]
fn process_pid_is_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(not(unix))]
fn process_pid_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct InteractivePidSnapshot {
    ppid: u32,
    tty: String,
    command: String,
}

#[cfg(unix)]
fn parse_pid_snapshot_line(line: &str) -> Option<InteractivePidSnapshot> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let ppid = parts.next()?.parse::<u32>().ok()?;
    let tty = parts.next()?.to_string();
    let command = parts.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    Some(InteractivePidSnapshot { ppid, tty, command })
}

#[cfg(unix)]
fn interactive_pid_snapshot(pid: u32) -> Option<InteractivePidSnapshot> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,tty=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    parse_pid_snapshot_line(line.as_ref())
}

#[cfg(unix)]
fn looks_like_interactive_hermes_process(command: &str) -> bool {
    let cmd = command.to_ascii_lowercase();
    (cmd.contains("hermes-agent-ultra") || cmd.contains("hermes-ultra")) && !cmd.contains("gateway")
}

#[cfg(unix)]
fn interactive_lock_holder_is_reapable_orphan(pid: u32) -> bool {
    let snapshot = match interactive_pid_snapshot(pid) {
        Some(snapshot) => snapshot,
        None => return false,
    };
    // Reap only obvious abandoned interactive agents:
    // orphaned from shell (ppid=1) and detached from a terminal.
    looks_like_interactive_hermes_process(&snapshot.command)
        && snapshot.ppid == 1
        && (snapshot.tty == "??" || snapshot.tty == "?")
}

#[cfg(unix)]
fn reap_interactive_orphan(pid: u32) -> bool {
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_millis(250));
    if !process_pid_is_alive(pid) {
        return true;
    }
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(150));
    !process_pid_is_alive(pid)
}

#[cfg(unix)]
fn reap_interactive_orphans_except(own_pid: u32) -> usize {
    let output = match std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return 0,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut reaped = 0usize;
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid == own_pid || ppid != 1 {
            continue;
        }
        let command = parts.collect::<Vec<_>>().join(" ");
        if looks_like_interactive_hermes_process(&command) && reap_interactive_orphan(pid) {
            reaped = reaped.saturating_add(1);
        }
    }
    reaped
}

struct InteractiveSessionLockGuard {
    lock_path: PathBuf,
    pid: u32,
    _lock_file: std::fs::File,
}

impl InteractiveSessionLockGuard {
    fn acquire(cli: &Cli) -> Result<Option<Self>, AgentError> {
        if hermes_config::env_var_enabled(INTERACTIVE_SESSION_LOCK_BYPASS_ENV) {
            return Ok(None);
        }
        let lock_path = interactive_lock_path_for_cli(cli);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!(
                    "failed to create lock parent {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }
        let own_pid = std::process::id();
        #[cfg(unix)]
        {
            let _ = reap_interactive_orphans_except(own_pid);
        }

        // Use create_new for atomic lock acquisition. This closes the race where
        // two interactive sessions read "no lock" and both write concurrently.
        let lock_file = loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => break file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(existing_pid) = read_interactive_lock_pid(&lock_path) {
                        if existing_pid != own_pid && process_pid_is_alive(existing_pid) {
                            #[cfg(unix)]
                            {
                                if interactive_lock_holder_is_reapable_orphan(existing_pid)
                                    && reap_interactive_orphan(existing_pid)
                                {
                                    let _ = std::fs::remove_file(&lock_path);
                                    continue;
                                }
                            }
                            return Err(AgentError::Config(format!(
                                "Another Hermes interactive session is running (PID {}). Close it first or set {}=1 to allow parallel sessions.",
                                existing_pid, INTERACTIVE_SESSION_LOCK_BYPASS_ENV
                            )));
                        }
                    }
                    let _ = std::fs::remove_file(&lock_path);
                    continue;
                }
                Err(err) => {
                    return Err(AgentError::Io(format!(
                        "failed to create interactive lock {}: {}",
                        lock_path.display(),
                        err
                    )));
                }
            }
        };

        let mut lock_file = lock_file;
        lock_file
            .write_all(format!("{}\n", own_pid).as_bytes())
            .map_err(|e| {
                AgentError::Io(format!(
                    "failed to write interactive lock {}: {}",
                    lock_path.display(),
                    e
                ))
            })?;
        let _ = lock_file.flush();

        Ok(Some(Self {
            lock_path,
            pid: own_pid,
            _lock_file: lock_file,
        }))
    }
}

impl Drop for InteractiveSessionLockGuard {
    fn drop(&mut self) {
        if let Some(current_pid) = read_interactive_lock_pid(&self.lock_path) {
            if current_pid == self.pid {
                let _ = std::fs::remove_file(&self.lock_path);
            }
        }
    }
}

/// Run the interactive REPL (default command).
async fn run_interactive(cli: Cli) -> Result<(), AgentError> {
    require_interactive_tty()?;
    let _session_lock = InteractiveSessionLockGuard::acquire(&cli)?;
    let app = App::new(cli).await?;
    hermes_cli::tui::run(app).await
}

fn run_kanban(args: Vec<String>) -> Result<(), AgentError> {
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    println!("{}", hermes_cli::commands::run_kanban_command(&arg_refs)?);
    Ok(())
}

#[derive(Debug, Clone)]
struct ResumeSessionPayload {
    resolved_id: String,
    source_path: PathBuf,
    session_id: String,
    model: Option<String>,
    personality: Option<String>,
    system_prompt: Option<String>,
    session_start: Option<String>,
    messages: Vec<hermes_core::Message>,
}

async fn run_resume(cli: Cli, requested_session_id: Option<String>) -> Result<(), AgentError> {
    require_interactive_tty()?;
    let _session_lock = InteractiveSessionLockGuard::acquire(&cli)?;
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
    let system_prompt = doc
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let session_start = doc
        .get("session_start")
        .and_then(|v| v.as_str())
        .or_else(|| {
            info.and_then(|v| v.get("created_at"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

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
        system_prompt,
        session_start,
        messages,
    })
}

fn legacy_sessions_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join(".hermes").join("sessions"))
}

fn resolve_resume_session_file_with_legacy_fallback(
    sessions_dir: &Path,
    requested: Option<&str>,
) -> Result<(String, PathBuf), AgentError> {
    match resolve_resume_session_file(sessions_dir, requested) {
        Ok(found) => Ok(found),
        Err(primary_err) => {
            let Some(legacy_dir) = legacy_sessions_dir() else {
                return Err(primary_err);
            };
            if legacy_dir == sessions_dir || !legacy_dir.exists() {
                return Err(primary_err);
            }
            resolve_resume_session_file(&legacy_dir, requested).map_err(|_| primary_err)
        }
    }
}

fn resolve_latest_nonempty_session_file_with_legacy_fallback(
    sessions_dir: &Path,
) -> Result<(String, PathBuf), AgentError> {
    match resolve_latest_nonempty_session_file(sessions_dir) {
        Ok(found) => Ok(found),
        Err(primary_err) => {
            let Some(legacy_dir) = legacy_sessions_dir() else {
                return Err(primary_err);
            };
            if legacy_dir == sessions_dir || !legacy_dir.exists() {
                return Err(primary_err);
            }
            resolve_latest_nonempty_session_file(&legacy_dir).map_err(|_| primary_err)
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
        // 2) newest canonical snapshot (may be startup empty snapshot)
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
                    let description = provider_picker_description(&entry.provider);
                    println!(
                        "  {:<18} - {} - {}{}{}",
                        entry.provider, description, preview, suffix, cap_suffix
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
        Some(other) => {
            println!(
                "Unknown tools action: {}. Use 'list', 'enable', or 'disable'.",
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
    println!(
        "Updated tools setup: {} enabled, {} disabled (config: {}).",
        enabled_known.len(),
        disabled_known.len(),
        cfg_path.display()
    );
    Ok(())
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
            let outcome = set_user_config_value(&base, &key, &value)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            match (outcome.config_path, outcome.env_path, outcome.env_key) {
                (Some(cfg_path), Some(env_path), Some(env_key)) => {
                    println!(
                        "Saved {} = {} -> {} and {} -> {}",
                        key,
                        value,
                        cfg_path.display(),
                        env_key,
                        env_path.display()
                    );
                }
                (Some(cfg_path), _, _) => {
                    println!("Saved {} = {} -> {}", key, value, cfg_path.display());
                }
                (_, Some(env_path), Some(env_key)) => {
                    println!("Saved {} -> {}", env_key, env_path.display());
                }
                _ => {}
            }
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

const ROUTE_AUTOTUNE_ENV_KEYS: &[&str] = &[
    "HERMES_SMART_ROUTING_LEARNING_ALPHA",
    "HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS",
    "HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN",
    "HERMES_SMART_ROUTING_LEARNING_TTL_SECS",
    "HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS",
];

fn route_autotune_env_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-autotune.env")
}

fn parse_simple_env_file(path: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, value)) = body.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        out.insert(key.to_string(), value);
    }
    out
}

fn apply_route_autotune_env_overrides(cli: &Cli) -> Vec<String> {
    let path = route_autotune_env_path_for_cli(cli);
    if !path.exists() {
        return Vec::new();
    }
    let parsed = parse_simple_env_file(&path);
    let mut applied = Vec::new();
    for key in ROUTE_AUTOTUNE_ENV_KEYS {
        if std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some()
        {
            continue;
        }
        if let Some(value) = parsed.get(*key) {
            std::env::set_var(key, value);
            applied.push((*key).to_string());
        }
    }
    applied
}

fn gateway_lock_path_for_pid_path(pid_path: &Path) -> PathBuf {
    pid_path.with_file_name("gateway.lock")
}

fn read_gateway_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(pid) = trimmed.parse::<u32>() {
        return Some(pid);
    }
    let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pid = json.get("pid")?.as_u64()?;
    u32::try_from(pid).ok()
}

fn cleanup_stale_gateway_metadata(pid_path: &Path) {
    let _ = std::fs::remove_file(pid_path);
    let _ = std::fs::remove_file(gateway_lock_path_for_pid_path(pid_path));
}

fn looks_like_gateway_process(cmdline: &str) -> bool {
    let cmdline = cmdline.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "hermes_cli.main gateway",
        "hermes_cli/main.py gateway",
        "hermes gateway",
        "hermes-agent-ultra gateway",
        "hermes-gateway",
        "gateway/run.py",
    ];
    PATTERNS.iter().any(|pattern| cmdline.contains(pattern))
}

#[cfg(unix)]
fn gateway_pid_commandline(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let cmdline = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cmdline.is_empty() {
        None
    } else {
        Some(cmdline)
    }
}

#[cfg(unix)]
fn gateway_pid_is_alive(pid: u32) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, 0) != 0 } {
        return false;
    }
    match gateway_pid_commandline(pid) {
        Some(cmdline) => looks_like_gateway_process(&cmdline),
        None => true,
    }
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

#[cfg(target_os = "macos")]
fn gateway_launchd_label() -> &'static str {
    "com.hermes_agent_ultra.gateway"
}

#[cfg(target_os = "macos")]
fn gateway_launchd_plist_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", gateway_launchd_label())),
    )
}

#[cfg(target_os = "macos")]
fn launchd_target() -> String {
    let uid = unsafe { libc::geteuid() };
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
fn launchctl_bootstrap(plist: &Path) -> Result<(), AgentError> {
    let target = launchd_target();
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &target])
        .arg(plist)
        .status();
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &target])
        .arg(plist)
        .status()
        .map_err(|e| AgentError::Io(format!("launchctl bootstrap: {e}")))?;
    if !status.success() {
        return Err(AgentError::Io(format!(
            "launchctl bootstrap failed for {}",
            plist.display()
        )));
    }
    let label = format!("{target}/{}", gateway_launchd_label());
    let _ = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &label])
        .status();
    Ok(())
}

fn install_gateway_service(force: bool, dry_run: bool) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Err(AgentError::Io(
                "unable to resolve launchd plist path".into(),
            ));
        };
        if plist_path.exists() && !force {
            println!(
                "Gateway service already installed at {} (use --force to overwrite).",
                plist_path.display()
            );
            return Ok(());
        }
        let agents_dir = plist_path
            .parent()
            .ok_or_else(|| AgentError::Io("invalid launch agents path".into()))?;
        if dry_run {
            println!(
                "Dry-run: would install gateway service plist at {}",
                plist_path.display()
            );
            return Ok(());
        }
        std::fs::create_dir_all(agents_dir)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {e}", agents_dir.display())))?;
        let exe = std::env::current_exe()
            .map_err(|e| AgentError::Io(format!("current_exe failed: {e}")))?;
        let logs_dir = hermes_home().join("logs");
        std::fs::create_dir_all(&logs_dir)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {e}", logs_dir.display())))?;
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{exe}</string>
      <string>gateway</string>
      <string>run</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{stdout}</string>
    <key>StandardErrorPath</key><string>{stderr}</string>
  </dict>
</plist>
"#,
            label = gateway_launchd_label(),
            exe = exe.display(),
            stdout = logs_dir.join("gateway-service.log").display(),
            stderr = logs_dir.join("gateway-service.err.log").display(),
        );
        std::fs::write(&plist_path, plist)
            .map_err(|e| AgentError::Io(format!("write {}: {e}", plist_path.display())))?;
        launchctl_bootstrap(&plist_path)?;
        println!(
            "Installed gateway launchd service at {}",
            plist_path.display()
        );
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (force, dry_run);
        println!("Gateway install is currently implemented for macOS launchd only.");
        Ok(())
    }
}

fn uninstall_gateway_service(dry_run: bool) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Err(AgentError::Io(
                "unable to resolve launchd plist path".into(),
            ));
        };
        if dry_run {
            println!(
                "Dry-run: would uninstall gateway service plist {}",
                plist_path.display()
            );
            return Ok(());
        }
        if plist_path.exists() {
            let target = launchd_target();
            let _ = std::process::Command::new("launchctl")
                .args(["bootout", &target])
                .arg(&plist_path)
                .status();
            std::fs::remove_file(&plist_path)
                .map_err(|e| AgentError::Io(format!("remove {}: {e}", plist_path.display())))?;
            println!("Removed gateway launchd service {}", plist_path.display());
        } else {
            println!("Gateway service is not installed.");
        }
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = dry_run;
        println!("Gateway uninstall is currently implemented for macOS launchd only.");
        Ok(())
    }
}

fn try_start_gateway_service() -> Result<bool, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(false);
        };
        if !plist_path.exists() {
            return Ok(false);
        }
        launchctl_bootstrap(&plist_path)?;
        return Ok(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

fn try_stop_gateway_service() -> Result<bool, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(false);
        };
        if !plist_path.exists() {
            return Ok(false);
        }
        let target = launchd_target();
        let status = std::process::Command::new("launchctl")
            .args(["bootout", &target])
            .arg(plist_path)
            .status()
            .map_err(|e| AgentError::Io(format!("launchctl bootout: {e}")))?;
        return Ok(status.success());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

fn try_restart_gateway_service() -> Result<bool, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(false);
        };
        if !plist_path.exists() {
            return Ok(false);
        }
        launchctl_bootstrap(&plist_path)?;
        return Ok(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

fn gateway_service_status() -> Result<Option<String>, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(None);
        };
        if !plist_path.exists() {
            return Ok(Some("Gateway service: not installed".to_string()));
        }
        let label = format!("{}/{}", launchd_target(), gateway_launchd_label());
        let out = std::process::Command::new("launchctl")
            .args(["print", &label])
            .output()
            .map_err(|e| AgentError::Io(format!("launchctl print: {e}")))?;
        if out.status.success() {
            return Ok(Some(format!(
                "Gateway service: installed (launchd label {}, running)",
                gateway_launchd_label()
            )));
        }
        Ok(Some(format!(
            "Gateway service: installed (launchd label {}, stopped)",
            gateway_launchd_label()
        )))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

fn migrate_legacy_gateway_services(dry_run: bool, yes: bool) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or_else(|| AgentError::Io("home dir not found".into()))?;
        let agents = home.join("Library").join("LaunchAgents");
        if !agents.exists() {
            println!("No LaunchAgents directory found; nothing to migrate.");
            return Ok(());
        }
        let mut legacy_plists: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&agents)
            .map_err(|e| AgentError::Io(format!("read {}: {e}", agents.display())))?
        {
            let entry = entry.map_err(|e| AgentError::Io(e.to_string()))?;
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let lower = file_name.to_ascii_lowercase();
            if lower.contains("hermes")
                && lower.contains("gateway")
                && file_name != format!("{}.plist", gateway_launchd_label())
            {
                legacy_plists.push(path);
            }
        }
        if legacy_plists.is_empty() {
            println!("No legacy gateway launchd units detected.");
            return Ok(());
        }
        println!("Legacy gateway units detected:");
        for p in &legacy_plists {
            println!("  - {}", p.display());
        }
        if !yes && !dry_run {
            return Err(AgentError::Config(
                "Refusing to remove legacy units without --yes (or use --dry-run).".into(),
            ));
        }
        if dry_run {
            println!("Dry-run complete; no files removed.");
            return Ok(());
        }
        let target = launchd_target();
        for p in legacy_plists {
            let _ = std::process::Command::new("launchctl")
                .args(["bootout", &target])
                .arg(&p)
                .status();
            let _ = std::fs::remove_file(&p);
            println!("Removed {}", p.display());
        }
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dry_run, yes);
        println!("Legacy gateway migration is currently implemented for macOS launchd only.");
        Ok(())
    }
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
            let pid_path = gateway_pid_path_for_cli(&cli);
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
            } else if gateway_allowlist_startup_would_warn(&config) {
                tracing::warn!(
                    "No gateway user allowlist or allow-all override configured; set platform *_ALLOWED_USERS or explicit *_ALLOW_ALL_USERS to silence this warning"
                );
                println!(
                    "Warning: no gateway user allowlist configured. Set platform *_ALLOWED_USERS or explicit *_ALLOW_ALL_USERS=true if this is intentional."
                );
            }
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
                cleanup_stale_gateway_metadata(&pid_path);
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
                display: config.display.clone(),
                service_tier: config.agent.normalized_service_tier(),
                quick_commands: config.quick_commands.clone(),
                kanban_dispatch_in_gateway: config.kanban.dispatch_in_gateway,
                ..RuntimeGatewayConfig::default()
            };
            let session_manager = Arc::new(SessionManager::new(config.session.clone()));
            let dm_manager = build_gateway_dm_manager(&config);
            let gateway = Arc::new(Gateway::new(
                session_manager,
                dm_manager,
                runtime_gateway_config,
            ));
            gateway
                .set_platform_access_policies(build_gateway_platform_access_policies(&config))
                .await;
            let mut hook_registry = HookRegistry::new();
            hook_registry.register_builtins();
            hook_registry.discover_and_load(&hermes_home().join("hooks"));
            gateway.set_hook_registry(Arc::new(hook_registry)).await;
            gateway
                .emit_hook_event(
                    "gateway:startup",
                    serde_json::json!({
                        "enabled_platforms": enabled.iter().map(|s| s.as_str()).collect::<Vec<_>>()
                    }),
                )
                .await;

            let tool_registry = Arc::new(ToolRegistry::new());
            let terminal_backend = build_terminal_backend(&config);
            let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
            let clarify_dispatcher = ClarifyDispatcher::new();
            let tool_registry_for_msg = tool_registry.clone();
            let tool_registry_for_stream = tool_registry.clone();
            let agent_tools_for_cron = Arc::new(bridge_tool_registry(&tool_registry));
            let clarify_for_msg = clarify_dispatcher.clone();
            let clarify_for_stream = clarify_dispatcher.clone();
            let config_arc = Arc::new(config.clone());
            let config_arc_stream = config_arc.clone();
            let gateway_for_review = gateway.clone();
            let gateway_for_review_stream = gateway.clone();
            gateway
                .set_message_handler_with_context(Arc::new(move |messages, ctx| {
                    let config = config_arc.clone();
                    let runtime_tools = tool_registry_for_msg.clone();
                    let gateway_for_review = gateway_for_review.clone();
                    let clarify = clarify_for_msg.clone();
                    Box::pin(async move {
                        if let Some(pending) = clarify.take_next().await {
                            let answer = messages
                                .iter()
                                .rev()
                                .find_map(|m| {
                                    (m.role == MessageRole::User)
                                        .then(|| m.content.clone())
                                        .flatten()
                                })
                                .unwrap_or_default();
                            let _ = pending.respond(answer);
                            return Ok(
                                "Clarification received. Continuing task execution...".to_string()
                            );
                        }
                        let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
                        let effective_model = resolve_model_for_gateway(
                            config.model.as_deref().unwrap_or("gpt-4o"),
                            &ctx,
                        );
                        let tool_schemas = resolve_platform_tool_schemas(
                            config.as_ref(),
                            &ctx.platform,
                            &runtime_tools,
                        );
                        let tool_defs = tool_definition_summary(&tool_schemas);
                        gateway_for_review
                            .emit_hook_event(
                                "agent:tool_definitions",
                                serde_json::json!({
                                    "platform": ctx.platform,
                                    "chat_id": ctx.chat_id,
                                    "user_id": ctx.user_id,
                                    "session_id": ctx.session_key,
                                    "streaming": false,
                                    "tools": tool_defs
                                }),
                            )
                            .await;
                        let platform_for_review = ctx.platform.clone();
                        let chat_for_review = ctx.chat_id.clone();
                        let deferred_queue = ctx.deferred_post_delivery_messages.clone();
                        let deferred_released = ctx.deferred_post_delivery_released.clone();
                        let gateway_for_review_cb = gateway_for_review.clone();
                        let review_cb = Arc::new(move |text: &str| {
                            if let (Some(queue), Some(released)) =
                                (deferred_queue.as_ref(), deferred_released.as_ref())
                            {
                                if !released.load(Ordering::Acquire) {
                                    if let Ok(mut guard) = queue.lock() {
                                        guard.push(text.to_string());
                                        return;
                                    }
                                }
                            }
                            let gw = gateway_for_review_cb.clone();
                            let platform = platform_for_review.clone();
                            let chat_id = chat_for_review.clone();
                            let msg = text.to_string();
                            tokio::spawn(async move {
                                let _ = gw.send_message(&platform, &chat_id, &msg, None).await;
                            });
                        });
                        let gateway_for_status = gateway_for_review.clone();
                        let gateway_for_status_hook = gateway_for_review.clone();
                        let platform_for_status = ctx.platform.clone();
                        let chat_for_status = ctx.chat_id.clone();
                        let platform_for_status_hook = ctx.platform.clone();
                        let user_for_status_hook = ctx.user_id.clone();
                        let session_for_status_hook = ctx.session_key.clone();
                        let status_cb = Arc::new(move |event_type: &str, message: &str| {
                            if message.trim().is_empty() {
                                return;
                            }
                            let gw = gateway_for_status.clone();
                            let platform = platform_for_status.clone();
                            let chat_id = chat_for_status.clone();
                            let status_key = event_type.to_string();
                            let msg = message.to_string();
                            tokio::spawn(async move {
                                let _ = gw
                                    .send_or_update_status(
                                        &platform,
                                        &chat_id,
                                        &status_key,
                                        &msg,
                                        None,
                                    )
                                    .await;
                            });
                            let gw_hook = gateway_for_status_hook.clone();
                            let platform = platform_for_status_hook.clone();
                            let user_id = user_for_status_hook.clone();
                            let session_id = session_for_status_hook.clone();
                            let event_type = event_type.to_string();
                            let message = message.to_string();
                            tokio::spawn(async move {
                                gw_hook
                                    .emit_hook_event(
                                        "agent:status",
                                        serde_json::json!({
                                            "platform": platform,
                                            "user_id": user_id,
                                            "session_id": session_id,
                                            "event_type": event_type,
                                            "message": message
                                        }),
                                    )
                                    .await;
                            });
                        });
                        let tool_events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
                        let tool_events_for_start = tool_events.clone();
                        let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
                            Box::new(move |name: &str, args: &serde_json::Value| {
                                let preview = build_tool_preview_from_value(name, args, 60)
                                    .unwrap_or_default();
                                let mut event = serde_json::json!({
                                    "phase": "start",
                                    "name": name,
                                    "emoji": tool_emoji(name)
                                });
                                if !preview.is_empty() {
                                    event["preview"] = serde_json::json!(preview);
                                }
                                if let Ok(mut guard) = tool_events_for_start.lock() {
                                    guard.push(event);
                                }
                            });
                        let tool_events_for_complete = tool_events.clone();
                        let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
                            Box::new(move |name: &str, result: &str| {
                                if let Ok(mut guard) = tool_events_for_complete.lock() {
                                    guard.push(serde_json::json!({
                                        "phase": "complete",
                                        "name": name,
                                        "emoji": tool_emoji(name),
                                        "result": truncate_hook_tool_result(result)
                                    }));
                                }
                            });
                        let tool_events_for_step = tool_events.clone();
                        let gateway_for_step_hook = gateway_for_review.clone();
                        let platform_for_step_hook = ctx.platform.clone();
                        let user_for_step_hook = ctx.user_id.clone();
                        let session_for_step_hook = ctx.session_key.clone();
                        let on_step_complete: Box<dyn Fn(u32) + Send + Sync> =
                            Box::new(move |iteration: u32| {
                                let tools = if let Ok(mut guard) = tool_events_for_step.lock() {
                                    std::mem::take(&mut *guard)
                                } else {
                                    Vec::new()
                                };
                                let tool_names: Vec<String> = tools
                                    .iter()
                                    .filter_map(|v| {
                                        v.get("name")
                                            .and_then(|n| n.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .collect();
                                let gw_hook = gateway_for_step_hook.clone();
                                let platform = platform_for_step_hook.clone();
                                let user_id = user_for_step_hook.clone();
                                let session_id = session_for_step_hook.clone();
                                tokio::spawn(async move {
                                    gw_hook
                                        .emit_hook_event(
                                            "agent:step",
                                            serde_json::json!({
                                                "platform": platform,
                                                "user_id": user_id,
                                                "session_id": session_id,
                                                "iteration": iteration,
                                                "tool_names": tool_names,
                                                "tools": tools
                                            }),
                                        )
                                        .await;
                                });
                            });
                        let callbacks = AgentCallbacks {
                            background_review_callback: Some(review_cb),
                            status_callback: Some(status_cb),
                            on_tool_start: Some(on_tool_start),
                            on_tool_complete: Some(on_tool_complete),
                            on_step_complete: Some(on_step_complete),
                            ..Default::default()
                        };
                        let agent =
                            build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools)
                                .with_callbacks(callbacks);
                        let result = agent
                            .run(messages, Some(tool_schemas))
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
                    let runtime_tools = tool_registry_for_stream.clone();
                    let gateway_for_review = gateway_for_review_stream.clone();
                    let clarify = clarify_for_stream.clone();
                    Box::pin(async move {
                        if let Some(pending) = clarify.take_next().await {
                            let answer = messages
                                .iter()
                                .rev()
                                .find_map(|m| {
                                    (m.role == MessageRole::User)
                                        .then(|| m.content.clone())
                                        .flatten()
                                })
                                .unwrap_or_default();
                            let _ = pending.respond(answer);
                            return Ok(
                                "Clarification received. Continuing task execution...".to_string()
                            );
                        }
                        let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
                        let effective_model = resolve_model_for_gateway(
                            config.model.as_deref().unwrap_or("gpt-4o"),
                            &ctx,
                        );
                        let tool_schemas = resolve_platform_tool_schemas(
                            config.as_ref(),
                            &ctx.platform,
                            &runtime_tools,
                        );
                        let tool_defs = tool_definition_summary(&tool_schemas);
                        gateway_for_review
                            .emit_hook_event(
                                "agent:tool_definitions",
                                serde_json::json!({
                                    "platform": ctx.platform,
                                    "chat_id": ctx.chat_id,
                                    "user_id": ctx.user_id,
                                    "session_id": ctx.session_key,
                                    "streaming": true,
                                    "tools": tool_defs
                                }),
                            )
                            .await;
                        let platform_for_review = ctx.platform.clone();
                        let chat_for_review = ctx.chat_id.clone();
                        let deferred_queue = ctx.deferred_post_delivery_messages.clone();
                        let deferred_released = ctx.deferred_post_delivery_released.clone();
                        let gateway_for_review_cb = gateway_for_review.clone();
                        let review_cb = Arc::new(move |text: &str| {
                            if let (Some(queue), Some(released)) =
                                (deferred_queue.as_ref(), deferred_released.as_ref())
                            {
                                if !released.load(Ordering::Acquire) {
                                    if let Ok(mut guard) = queue.lock() {
                                        guard.push(text.to_string());
                                        return;
                                    }
                                }
                            }
                            let gw = gateway_for_review_cb.clone();
                            let platform = platform_for_review.clone();
                            let chat_id = chat_for_review.clone();
                            let msg = text.to_string();
                            tokio::spawn(async move {
                                let _ = gw.send_message(&platform, &chat_id, &msg, None).await;
                            });
                        });
                        let gateway_for_status = gateway_for_review.clone();
                        let gateway_for_status_hook = gateway_for_review.clone();
                        let platform_for_status = ctx.platform.clone();
                        let chat_for_status = ctx.chat_id.clone();
                        let platform_for_status_hook = ctx.platform.clone();
                        let user_for_status_hook = ctx.user_id.clone();
                        let session_for_status_hook = ctx.session_key.clone();
                        let status_cb = Arc::new(move |event_type: &str, message: &str| {
                            if message.trim().is_empty() {
                                return;
                            }
                            let gw = gateway_for_status.clone();
                            let platform = platform_for_status.clone();
                            let chat_id = chat_for_status.clone();
                            let status_key = event_type.to_string();
                            let msg = message.to_string();
                            tokio::spawn(async move {
                                let _ = gw
                                    .send_or_update_status(
                                        &platform,
                                        &chat_id,
                                        &status_key,
                                        &msg,
                                        None,
                                    )
                                    .await;
                            });
                            let gw_hook = gateway_for_status_hook.clone();
                            let platform = platform_for_status_hook.clone();
                            let user_id = user_for_status_hook.clone();
                            let session_id = session_for_status_hook.clone();
                            let event_type = event_type.to_string();
                            let message = message.to_string();
                            tokio::spawn(async move {
                                gw_hook
                                    .emit_hook_event(
                                        "agent:status",
                                        serde_json::json!({
                                            "platform": platform,
                                            "user_id": user_id,
                                            "session_id": session_id,
                                            "event_type": event_type,
                                            "message": message
                                        }),
                                    )
                                    .await;
                            });
                        });
                        let tool_events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
                        let tool_events_for_start = tool_events.clone();
                        let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
                            Box::new(move |name: &str, args: &serde_json::Value| {
                                let preview = build_tool_preview_from_value(name, args, 60)
                                    .unwrap_or_default();
                                let mut event = serde_json::json!({
                                    "phase": "start",
                                    "name": name,
                                    "emoji": tool_emoji(name)
                                });
                                if !preview.is_empty() {
                                    event["preview"] = serde_json::json!(preview);
                                }
                                if let Ok(mut guard) = tool_events_for_start.lock() {
                                    guard.push(event);
                                }
                            });
                        let tool_events_for_complete = tool_events.clone();
                        let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
                            Box::new(move |name: &str, result: &str| {
                                if let Ok(mut guard) = tool_events_for_complete.lock() {
                                    guard.push(serde_json::json!({
                                        "phase": "complete",
                                        "name": name,
                                        "emoji": tool_emoji(name),
                                        "result": truncate_hook_tool_result(result)
                                    }));
                                }
                            });
                        let tool_events_for_step = tool_events.clone();
                        let gateway_for_step_hook = gateway_for_review.clone();
                        let platform_for_step_hook = ctx.platform.clone();
                        let user_for_step_hook = ctx.user_id.clone();
                        let session_for_step_hook = ctx.session_key.clone();
                        let on_step_complete: Box<dyn Fn(u32) + Send + Sync> =
                            Box::new(move |iteration: u32| {
                                let tools = if let Ok(mut guard) = tool_events_for_step.lock() {
                                    std::mem::take(&mut *guard)
                                } else {
                                    Vec::new()
                                };
                                let tool_names: Vec<String> = tools
                                    .iter()
                                    .filter_map(|v| {
                                        v.get("name")
                                            .and_then(|n| n.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .collect();
                                let gw_hook = gateway_for_step_hook.clone();
                                let platform = platform_for_step_hook.clone();
                                let user_id = user_for_step_hook.clone();
                                let session_id = session_for_step_hook.clone();
                                tokio::spawn(async move {
                                    gw_hook
                                        .emit_hook_event(
                                            "agent:step",
                                            serde_json::json!({
                                                "platform": platform,
                                                "user_id": user_id,
                                                "session_id": session_id,
                                                "iteration": iteration,
                                                "tool_names": tool_names,
                                                "tools": tools
                                            }),
                                        )
                                        .await;
                                });
                            });
                        let callbacks = AgentCallbacks {
                            background_review_callback: Some(review_cb),
                            status_callback: Some(status_cb),
                            on_tool_start: Some(on_tool_start),
                            on_tool_complete: Some(on_tool_complete),
                            on_step_complete: Some(on_step_complete),
                            ..Default::default()
                        };
                        let agent =
                            build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools)
                                .with_callbacks(callbacks);
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
                            .run_stream(messages, Some(tool_schemas), Some(stream_cb))
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
            wire_cron_scheduler_backend(&tool_registry, cron_scheduler.clone());
            wire_gateway_messaging_backend(&tool_registry, gateway.clone());
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

            let mut sidecar_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            let webhooks_path_clone = webhooks_path.clone();
            sidecar_tasks.push(tokio::spawn(async move {
                run_cron_webhook_delivery_loop(cron_rx, webhooks_path_clone).await;
            }));

            register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks).await?;

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
            if let Some(service_state) = gateway_service_status()? {
                println!("{service_state}");
            }
            let pid_path = gateway_pid_path_for_cli(&cli);
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
            let pid_path = gateway_pid_path_for_cli(&cli);
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

fn run_sessions_db_auto_maintenance(config: &GatewayConfig) {
    if !config.sessions.auto_prune {
        return;
    }
    let home = config
        .home_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(hermes_home);
    let sp = SessionPersistence::new(&home);
    let result = sp.maybe_auto_prune_and_vacuum(
        config.sessions.retention_days,
        config.sessions.min_interval_hours,
        config.sessions.vacuum_after_prune,
    );
    if let Some(err) = result.error {
        tracing::debug!("sessions db auto-maintenance skipped: {}", err);
    } else if !result.skipped && result.pruned > 0 {
        tracing::info!(
            "sessions db auto-maintenance pruned {} session(s){}",
            result.pruned,
            if result.vacuumed { " + vacuum" } else { "" }
        );
    }
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

fn enabled_flag(platform: Option<&PlatformConfig>) -> &'static str {
    if platform.map(|p| p.enabled).unwrap_or(false) {
        "enabled"
    } else {
        "disabled"
    }
}

fn set_extra_string_if_nonempty(platform: &mut PlatformConfig, key: &str, value: &str) {
    let v = value.trim();
    if !v.is_empty() {
        platform
            .extra
            .insert(key.to_string(), serde_json::Value::String(v.to_string()));
    }
}

async fn configure_platform_basic_prompts(
    disk: &mut hermes_config::GatewayConfig,
    key: &str,
) -> Result<(), AgentError> {
    let p = disk
        .platforms
        .entry(key.to_string())
        .or_insert_with(PlatformConfig::default);
    p.enabled = true;

    match key {
        "discord" => {
            let token = prompt_line("Discord bot token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let app_id = prompt_line("Discord application_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "application_id", &app_id);
            let allowed =
                prompt_line("Discord allowed users (comma-separated, optional): ").await?;
            if !allowed.trim().is_empty() {
                p.allowed_users = parse_csv_list(&allowed);
            }
            let home = prompt_line("Discord home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "slack" => {
            let token = prompt_line("Slack bot token (xoxb-...): ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let app_token = prompt_line("Slack app token (xapp-..., optional): ").await?;
            set_extra_string_if_nonempty(p, "app_token", &app_token);
            let socket_mode = prompt_yes_no("Slack use socket_mode?", true).await?;
            p.extra.insert(
                "socket_mode".to_string(),
                serde_json::Value::Bool(socket_mode),
            );
        }
        "matrix" => {
            let homeserver =
                prompt_line("Matrix homeserver_url (e.g. https://matrix.org): ").await?;
            set_extra_string_if_nonempty(p, "homeserver_url", &homeserver);
            let user_id = prompt_line("Matrix user_id (e.g. @bot:matrix.org): ").await?;
            set_extra_string_if_nonempty(p, "user_id", &user_id);
            let token = prompt_line("Matrix access token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let room = prompt_line("Matrix home room_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "room_id", &room);
        }
        "mattermost" => {
            let server_url = prompt_line("Mattermost server_url: ").await?;
            set_extra_string_if_nonempty(p, "server_url", &server_url);
            let token = prompt_line("Mattermost bot token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let team_id = prompt_line("Mattermost team_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "team_id", &team_id);
            let home = prompt_line("Mattermost home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "signal" => {
            let account = prompt_line("Signal phone_number/account (e.g. +15551234567): ").await?;
            set_extra_string_if_nonempty(p, "phone_number", &account);
            let api_url = prompt_line("Signal api_url (default http://localhost:8080): ").await?;
            set_extra_string_if_nonempty(p, "api_url", &api_url);
        }
        "whatsapp" => {
            let token = prompt_line("WhatsApp Cloud API token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let phone_id = prompt_line("WhatsApp phone_number_id: ").await?;
            set_extra_string_if_nonempty(p, "phone_number_id", &phone_id);
            let verify = prompt_line("WhatsApp verify_token (optional): ").await?;
            set_extra_string_if_nonempty(p, "verify_token", &verify);
            let home = prompt_line("WhatsApp home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "dingtalk" => {
            let client_id = prompt_line("DingTalk client_id/appkey: ").await?;
            set_extra_string_if_nonempty(p, "client_id", &client_id);
            let client_secret = prompt_line("DingTalk client_secret: ").await?;
            set_extra_string_if_nonempty(p, "client_secret", &client_secret);
        }
        "feishu" => {
            let app_id = prompt_line("Feishu/Lark app_id: ").await?;
            set_extra_string_if_nonempty(p, "app_id", &app_id);
            let app_secret = prompt_line("Feishu/Lark app_secret: ").await?;
            set_extra_string_if_nonempty(p, "app_secret", &app_secret);
            let verify = prompt_line("Feishu verification_token (optional): ").await?;
            set_extra_string_if_nonempty(p, "verification_token", &verify);
            let encrypt_key = prompt_line("Feishu encrypt_key (optional): ").await?;
            set_extra_string_if_nonempty(p, "encrypt_key", &encrypt_key);
        }
        "wecom" => {
            let corp_id = prompt_line("WeCom corp_id: ").await?;
            set_extra_string_if_nonempty(p, "corp_id", &corp_id);
            let agent_id = prompt_line("WeCom agent_id: ").await?;
            set_extra_string_if_nonempty(p, "agent_id", &agent_id);
            let secret = prompt_line("WeCom secret: ").await?;
            set_extra_string_if_nonempty(p, "secret", &secret);
        }
        "wecom_callback" => {
            let corp_id = prompt_line("WeCom callback corp_id: ").await?;
            set_extra_string_if_nonempty(p, "corp_id", &corp_id);
            let corp_secret = prompt_line("WeCom callback corp_secret: ").await?;
            set_extra_string_if_nonempty(p, "corp_secret", &corp_secret);
            let agent_id = prompt_line("WeCom callback agent_id: ").await?;
            set_extra_string_if_nonempty(p, "agent_id", &agent_id);
            let token = prompt_line("WeCom callback token: ").await?;
            set_extra_string_if_nonempty(p, "token", &token);
            let aes = prompt_line("WeCom callback encoding_aes_key: ").await?;
            set_extra_string_if_nonempty(p, "encoding_aes_key", &aes);
            let host = prompt_line("WeCom callback host (default 0.0.0.0): ").await?;
            set_extra_string_if_nonempty(p, "host", &host);
            let port = prompt_line("WeCom callback port (default 8645): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let path = prompt_line("WeCom callback path (default /wecom/callback): ").await?;
            set_extra_string_if_nonempty(p, "path", &path);
        }
        "qqbot" => {
            let app_id = prompt_line("QQBot app_id: ").await?;
            set_extra_string_if_nonempty(p, "app_id", &app_id);
            let secret = prompt_line("QQBot client_secret: ").await?;
            set_extra_string_if_nonempty(p, "client_secret", &secret);
            let markdown = prompt_yes_no("QQBot markdown_support?", true).await?;
            p.extra.insert(
                "markdown_support".to_string(),
                serde_json::Value::Bool(markdown),
            );
        }
        "bluebubbles" => {
            let server_url = prompt_line("BlueBubbles server_url: ").await?;
            set_extra_string_if_nonempty(p, "server_url", &server_url);
            let password = prompt_line("BlueBubbles password: ").await?;
            set_extra_string_if_nonempty(p, "password", &password);
        }
        "email" => {
            let username = prompt_line("Email username/address: ").await?;
            set_extra_string_if_nonempty(p, "username", &username);
            let password = prompt_line("Email password/app password: ").await?;
            set_extra_string_if_nonempty(p, "password", &password);
            let imap_host = prompt_line("Email imap_host: ").await?;
            set_extra_string_if_nonempty(p, "imap_host", &imap_host);
            let smtp_host = prompt_line("Email smtp_host: ").await?;
            set_extra_string_if_nonempty(p, "smtp_host", &smtp_host);
            let imap_port = prompt_line("Email imap_port (default 993): ").await?;
            if let Ok(v) = imap_port.trim().parse::<u16>() {
                p.extra
                    .insert("imap_port".to_string(), serde_json::Value::from(v));
            }
            let smtp_port = prompt_line("Email smtp_port (default 587): ").await?;
            if let Ok(v) = smtp_port.trim().parse::<u16>() {
                p.extra
                    .insert("smtp_port".to_string(), serde_json::Value::from(v));
            }
        }
        "sms" => {
            let sid = prompt_line("Twilio account_sid: ").await?;
            set_extra_string_if_nonempty(p, "account_sid", &sid);
            let auth = prompt_line("Twilio auth_token: ").await?;
            set_extra_string_if_nonempty(p, "auth_token", &auth);
            let from = prompt_line("Twilio from_number (E.164): ").await?;
            set_extra_string_if_nonempty(p, "from_number", &from);
        }
        "homeassistant" => {
            let base_url =
                prompt_line("HomeAssistant base_url (e.g. http://127.0.0.1:8123): ").await?;
            set_extra_string_if_nonempty(p, "base_url", &base_url);
            let token = prompt_line("HomeAssistant long_lived_token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let webhook_id = prompt_line("HomeAssistant webhook_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "webhook_id", &webhook_id);
        }
        "ntfy" => {
            let topic = prompt_line("ntfy subscribe topic: ").await?;
            set_extra_string_if_nonempty(p, "topic", &topic);
            let server = prompt_line("ntfy server URL (default https://ntfy.sh): ").await?;
            set_extra_string_if_nonempty(p, "server", &server);
            let publish_topic = prompt_line("ntfy publish topic (optional): ").await?;
            set_extra_string_if_nonempty(p, "publish_topic", &publish_topic);
            let token = prompt_line("ntfy auth token (optional): ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
        }
        "webhook" => {
            let secret = prompt_line("Webhook secret: ").await?;
            set_extra_string_if_nonempty(p, "secret", &secret);
            let port = prompt_line("Webhook port (default 9000): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let path = prompt_line("Webhook path (default /webhook): ").await?;
            set_extra_string_if_nonempty(p, "path", &path);
        }
        "api_server" => {
            let host = prompt_line("API server host (default 127.0.0.1): ").await?;
            set_extra_string_if_nonempty(p, "host", &host);
            let port = prompt_line("API server port (default 8090): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let token =
                prompt_line("API server auth_token (required for non-loopback host): ").await?;
            set_extra_string_if_nonempty(p, "auth_token", &token);
        }
        _ => {}
    }
    Ok(())
}

async fn run_gateway_setup(cli: &Cli) -> Result<(), AgentError> {
    println!("Gateway setup wizard");
    println!("--------------------");
    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    let platform_catalog: &[(&str, &str)] = &[
        ("weixin", "Weixin"),
        ("qqbot", "QQBot"),
        ("telegram", "Telegram"),
        ("discord", "Discord"),
        ("slack", "Slack"),
        ("matrix", "Matrix"),
        ("mattermost", "Mattermost"),
        ("whatsapp", "WhatsApp"),
        ("signal", "Signal"),
        ("dingtalk", "DingTalk"),
        ("feishu", "Feishu"),
        ("wecom", "WeCom"),
        ("wecom_callback", "WeCom Callback"),
        ("bluebubbles", "BlueBubbles"),
        ("email", "Email"),
        ("sms", "SMS"),
        ("homeassistant", "HomeAssistant"),
        ("ntfy", "ntfy"),
        ("webhook", "Webhook"),
        ("api_server", "API Server"),
    ];
    println!("This wizard configures messaging platforms in config.yaml.");
    println!("Current platform status:");
    for (k, label) in platform_catalog {
        println!("  - {:<13} {}", label, enabled_flag(disk.platforms.get(*k)));
    }
    println!();
    println!("Use SPACE to toggle platforms and ENTER to confirm.");
    let mut pre_selected: HashSet<usize> = HashSet::new();
    for (idx, (key, _)) in platform_catalog.iter().enumerate() {
        if disk
            .platforms
            .get(*key)
            .map(|cfg| cfg.enabled)
            .unwrap_or(false)
        {
            pre_selected.insert(idx);
        }
    }
    let selection_items: Vec<String> = platform_catalog
        .iter()
        .map(|(key, label)| format!("{:<13} {}", label, enabled_flag(disk.platforms.get(*key))))
        .collect();
    let selected_result = hermes_cli::curses_checklist(
        "Select platforms to configure",
        &selection_items,
        &pre_selected,
        Some(&|selected| {
            if selected.is_empty() {
                "none selected".to_string()
            } else {
                format!("{} selected", selected.len())
            }
        }),
    );
    if !selected_result.confirmed {
        println!("Gateway setup cancelled.");
        return Ok(());
    }
    let mut selected: Vec<String> = selected_result
        .selected
        .iter()
        .copied()
        .filter_map(|idx| platform_catalog.get(idx).map(|(key, _)| key.to_string()))
        .collect();
    selected.sort();
    selected.dedup();
    if selected.is_empty() {
        println!("No valid platforms selected.");
        return Ok(());
    }

    for key in selected {
        println!();
        println!("Configuring {}...", key);
        match key.as_str() {
            "weixin" => {
                run_auth(
                    cli.clone(),
                    Some("login".to_string()),
                    Some("weixin".to_string()),
                    None,
                    None,
                    None,
                    None,
                    true,
                )
                .await?;
                disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let wx = disk
                    .platforms
                    .entry("weixin".to_string())
                    .or_insert_with(PlatformConfig::default);
                wx.enabled = true;
                println!("Direct message policy: 1)pairing 2)open 3)allowlist 4)disabled");
                let dm_choice = prompt_line("Choose [1-4] (default 1): ").await?;
                match dm_choice.trim() {
                    "2" => {
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("open"));
                        wx.extra
                            .insert("allow_from".to_string(), serde_json::json!([]));
                    }
                    "3" => {
                        let ids = parse_csv_list(
                            &prompt_line("Allowed Weixin user IDs (comma-separated): ").await?,
                        );
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("allowlist"));
                        wx.extra.insert(
                            "allow_from".to_string(),
                            serde_json::Value::Array(
                                ids.into_iter().map(serde_json::Value::String).collect(),
                            ),
                        );
                    }
                    "4" => {
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("disabled"));
                        wx.extra
                            .insert("allow_from".to_string(), serde_json::json!([]));
                    }
                    _ => {
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("pairing"));
                        wx.extra
                            .insert("allow_from".to_string(), serde_json::json!([]));
                    }
                }
                println!("Group policy: 1)disabled 2)open 3)allowlist");
                let group_choice = prompt_line("Choose [1-3] (default 1): ").await?;
                match group_choice.trim() {
                    "2" => {
                        wx.extra
                            .insert("group_policy".to_string(), serde_json::json!("open"));
                        wx.extra
                            .insert("group_allow_from".to_string(), serde_json::json!([]));
                    }
                    "3" => {
                        let ids = parse_csv_list(
                            &prompt_line("Allowed Weixin group IDs (comma-separated): ").await?,
                        );
                        wx.extra
                            .insert("group_policy".to_string(), serde_json::json!("allowlist"));
                        wx.extra.insert(
                            "group_allow_from".to_string(),
                            serde_json::Value::Array(
                                ids.into_iter().map(serde_json::Value::String).collect(),
                            ),
                        );
                    }
                    _ => {
                        wx.extra
                            .insert("group_policy".to_string(), serde_json::json!("disabled"));
                        wx.extra
                            .insert("group_allow_from".to_string(), serde_json::json!([]));
                    }
                }
                let home = prompt_line("Weixin home channel (optional): ").await?;
                if !home.trim().is_empty() {
                    wx.home_channel = Some(home.trim().to_string());
                }
            }
            "telegram" => {
                run_auth(
                    cli.clone(),
                    Some("login".to_string()),
                    Some("telegram".to_string()),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await?;
                disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let tg = disk
                    .platforms
                    .entry("telegram".to_string())
                    .or_insert_with(PlatformConfig::default);
                tg.enabled = true;
                let polling = prompt_yes_no("Telegram use polling mode?", true).await?;
                tg.extra
                    .insert("polling".to_string(), serde_json::Value::Bool(polling));
                if !polling {
                    let webhook_url = prompt_line("Telegram webhook URL: ").await?;
                    if !webhook_url.trim().is_empty() {
                        tg.webhook_url = Some(webhook_url.trim().to_string());
                    }
                }
                let home = prompt_line("Telegram home channel (optional): ").await?;
                if !home.trim().is_empty() {
                    tg.home_channel = Some(home.trim().to_string());
                }
            }
            other => configure_platform_basic_prompts(&mut disk, other).await?,
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
    if let Some(service_tier) = ctx.service_tier.clone() {
        let mut extra = match agent_config.extra_body.take() {
            Some(serde_json::Value::Object(map)) => map,
            Some(other) => {
                let mut map = serde_json::Map::new();
                map.insert("extra_body".to_string(), other);
                map
            }
            None => serde_json::Map::new(),
        };
        extra.insert(
            "service_tier".to_string(),
            serde_json::Value::String(service_tier),
        );
        agent_config.extra_body = Some(serde_json::Value::Object(extra));
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
    hermes_agent::attach_discovered_memory(AgentLoop::new(agent_config, agent_tools, provider))
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

fn truncate_hook_tool_result(result: &str) -> String {
    let trimmed = result.trim();
    if trimmed.chars().count() <= 240 {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(240).collect();
    format!("{prefix}...")
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
        webhook_secret: extra_string(platform_cfg, "webhook_secret")
            .or_else(|| std::env::var("TELEGRAM_WEBHOOK_SECRET").ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        polling,
        proxy: Default::default(),
        parse_markdown,
        parse_html,
        poll_timeout,
        reply_to_mode: reply_to_mode_string(platform_cfg).unwrap_or_else(|| "first".to_string()),
        reactions: extra_bool(platform_cfg, "reactions", false),
        fallback_ips: extra_string_vec(platform_cfg, "fallback_ips"),
        require_mention: platform_cfg
            .require_mention
            .or_else(|| extra_bool_loose(platform_cfg, "require_mention"))
            .unwrap_or(false),
        guest_mode: extra_bool(platform_cfg, "guest_mode", false),
        free_response_chats: extra_string_vec(platform_cfg, "free_response_chats"),
        allowed_chats: extra_string_vec(platform_cfg, "allowed_chats"),
        group_allowed_chats: extra_string_vec(platform_cfg, "group_allowed_chats"),
        ignored_threads: extra_string_vec(platform_cfg, "ignored_threads"),
        allowed_topics: extra_string_vec(platform_cfg, "allowed_topics"),
        mention_patterns: extra_string_vec(platform_cfg, "mention_patterns"),
        exclusive_bot_mentions: extra_bool(platform_cfg, "exclusive_bot_mentions", false),
        observe_unmentioned_group_messages: extra_bool(
            platform_cfg,
            "observe_unmentioned_group_messages",
            false,
        ),
        text_batch_delay_ms: platform_cfg
            .extra
            .get("text_batch_delay_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(750),
        bot_username: None,
    }
}

fn platform_token_or_extra(platform_cfg: &PlatformConfig) -> Option<String> {
    platform_cfg
        .token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            platform_cfg
                .extra
                .get("token")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
}

fn extra_string(platform_cfg: &PlatformConfig, key: &str) -> Option<String> {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn extra_string_set(platform_cfg: &PlatformConfig, key: &str) -> HashSet<String> {
    let Some(raw) = platform_cfg.extra.get(key) else {
        return HashSet::new();
    };

    let mut values = HashSet::new();
    match raw {
        serde_json::Value::String(s) => {
            for item in s.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                values.insert(item.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            values.insert(trimmed.to_string());
                        }
                    }
                    serde_json::Value::Number(n) => {
                        values.insert(n.to_string());
                    }
                    _ => {}
                }
            }
        }
        serde_json::Value::Number(n) => {
            values.insert(n.to_string());
        }
        _ => {}
    }
    values
}

fn extra_string_vec(platform_cfg: &PlatformConfig, key: &str) -> Vec<String> {
    let Some(raw) = platform_cfg.extra.get(key) else {
        return Vec::new();
    };
    match raw {
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .flat_map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect(),
        serde_json::Value::String(s) => s
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        serde_json::Value::Number(n) => vec![n.to_string()],
        _ => Vec::new(),
    }
}

fn discord_reply_to_mode_string(platform_cfg: &PlatformConfig) -> Option<String> {
    reply_to_mode_string(platform_cfg)
}

fn reply_to_mode_string(platform_cfg: &PlatformConfig) -> Option<String> {
    let raw = platform_cfg.extra.get("reply_to_mode")?;
    let candidate = match raw {
        serde_json::Value::String(value) => value.trim().to_ascii_lowercase(),
        serde_json::Value::Bool(false) => "off".to_string(),
        serde_json::Value::Bool(true) => "all".to_string(),
        _ => return None,
    };

    matches!(candidate.as_str(), "off" | "first" | "all").then_some(candidate)
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn matrix_home_room_for_platform(platform_cfg: &PlatformConfig) -> Option<String> {
    extra_string(platform_cfg, "room_id")
        .or_else(|| extra_string(platform_cfg, "home_room"))
        .or_else(|| env_string("MATRIX_HOME_ROOM"))
}

fn extra_bool(platform_cfg: &PlatformConfig, key: &str, default: bool) -> bool {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

fn extra_u16(platform_cfg: &PlatformConfig, key: &str, default: u16) -> u16 {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(default)
}

fn extra_bool_loose(platform_cfg: &PlatformConfig, key: &str) -> Option<bool> {
    let raw = platform_cfg.extra.get(key)?;
    if let Some(v) = raw.as_bool() {
        return Some(v);
    }
    raw.as_str().and_then(|v| {
        let normalized = v.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "1" | "true" | "yes" | "y" | "on" | "enable" | "enabled" => Some(true),
            "0" | "false" | "no" | "n" | "off" | "disable" | "disabled" => Some(false),
            _ => None,
        }
    })
}

fn discord_allow_bots_bypasses_gateway_allowlist(
    platform: &str,
    platform_cfg: &PlatformConfig,
) -> bool {
    if !platform.eq_ignore_ascii_case("discord") {
        return false;
    }
    extra_string(platform_cfg, "allow_bots")
        .map(|raw| matches!(raw.trim().to_ascii_lowercase().as_str(), "all" | "mentions"))
        .unwrap_or(false)
}

#[derive(Clone, Copy)]
struct PlatformGatewayAuthEnv {
    platform: &'static str,
    allowed_users: &'static str,
    allow_all_users: &'static str,
    group_allowed_users: Option<&'static str>,
    group_allowed_chats: Option<&'static str>,
}

const PLATFORM_GATEWAY_AUTH_ENVS: &[PlatformGatewayAuthEnv] = &[
    PlatformGatewayAuthEnv {
        platform: "telegram",
        allowed_users: "TELEGRAM_ALLOWED_USERS",
        allow_all_users: "TELEGRAM_ALLOW_ALL_USERS",
        group_allowed_users: Some("TELEGRAM_GROUP_ALLOWED_USERS"),
        group_allowed_chats: Some("TELEGRAM_GROUP_ALLOWED_CHATS"),
    },
    PlatformGatewayAuthEnv {
        platform: "discord",
        allowed_users: "DISCORD_ALLOWED_USERS",
        allow_all_users: "DISCORD_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "whatsapp",
        allowed_users: "WHATSAPP_ALLOWED_USERS",
        allow_all_users: "WHATSAPP_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "slack",
        allowed_users: "SLACK_ALLOWED_USERS",
        allow_all_users: "SLACK_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "signal",
        allowed_users: "SIGNAL_ALLOWED_USERS",
        allow_all_users: "SIGNAL_ALLOW_ALL_USERS",
        group_allowed_users: Some("SIGNAL_GROUP_ALLOWED_USERS"),
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "email",
        allowed_users: "EMAIL_ALLOWED_USERS",
        allow_all_users: "EMAIL_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "sms",
        allowed_users: "SMS_ALLOWED_USERS",
        allow_all_users: "SMS_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "mattermost",
        allowed_users: "MATTERMOST_ALLOWED_USERS",
        allow_all_users: "MATTERMOST_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "matrix",
        allowed_users: "MATRIX_ALLOWED_USERS",
        allow_all_users: "MATRIX_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "dingtalk",
        allowed_users: "DINGTALK_ALLOWED_USERS",
        allow_all_users: "DINGTALK_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "feishu",
        allowed_users: "FEISHU_ALLOWED_USERS",
        allow_all_users: "FEISHU_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "wecom",
        allowed_users: "WECOM_ALLOWED_USERS",
        allow_all_users: "WECOM_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: None,
    },
    PlatformGatewayAuthEnv {
        platform: "qqbot",
        allowed_users: "QQ_ALLOWED_USERS",
        allow_all_users: "QQ_ALLOW_ALL_USERS",
        group_allowed_users: None,
        group_allowed_chats: Some("QQ_GROUP_ALLOWED_USERS"),
    },
];

fn canonical_gateway_platform(platform: &str) -> String {
    let platform = platform.trim().to_ascii_lowercase();
    match platform.as_str() {
        "qq" | "qq_bot" => "qqbot".to_string(),
        _ => platform,
    }
}

fn platform_gateway_auth_env(platform: &str) -> Option<PlatformGatewayAuthEnv> {
    let platform = canonical_gateway_platform(platform);
    PLATFORM_GATEWAY_AUTH_ENVS
        .iter()
        .copied()
        .find(|entry| entry.platform == platform)
}

fn env_list_from_lookup<F>(lookup: &mut F, key: &str) -> HashSet<String>
where
    F: FnMut(&str) -> Option<String>,
{
    lookup(key)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn env_truthy_from_lookup<F>(lookup: &mut F, key: &str) -> bool
where
    F: FnMut(&str) -> Option<String>,
{
    lookup(key).is_some_and(|value| env_value_truthy(&value))
}

fn dm_policy_unauthorized_behavior(raw: &str) -> Option<UnauthorizedDmBehavior> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pairing" | "pair" => Some(UnauthorizedDmBehavior::Pair),
        "allowlist" | "disabled" | "ignore" | "deny" | "drop" => {
            Some(UnauthorizedDmBehavior::Ignore)
        }
        _ => None,
    }
}

fn platform_dm_policy_env_key(platform: &str) -> String {
    format!("{}_DM_POLICY", platform.to_ascii_uppercase())
}

fn explicit_platform_unauthorized_dm_behavior<F>(
    platform: &str,
    platform_cfg: &PlatformConfig,
    lookup: &mut F,
) -> Option<UnauthorizedDmBehavior>
where
    F: FnMut(&str) -> Option<String>,
{
    if let Some(raw) = extra_string(platform_cfg, "unauthorized_dm_behavior") {
        return match raw.trim().to_ascii_lowercase().as_str() {
            "pair" => Some(UnauthorizedDmBehavior::Pair),
            "ignore" | "deny" | "drop" => Some(UnauthorizedDmBehavior::Ignore),
            _ => None,
        };
    }
    if let Some(raw) = extra_string(platform_cfg, "dm_policy")
        .or_else(|| lookup(&platform_dm_policy_env_key(platform)))
    {
        if let Some(behavior) = dm_policy_unauthorized_behavior(&raw) {
            return Some(behavior);
        }
    }
    (platform_cfg.unauthorized_dm_behavior == UnauthorizedDmBehavior::Pair)
        .then_some(UnauthorizedDmBehavior::Pair)
}

fn split_group_authorization_values(
    platform: &str,
    values: impl IntoIterator<Item = String>,
) -> (HashSet<String>, HashSet<String>) {
    let platform = canonical_gateway_platform(platform);
    let mut users = HashSet::new();
    let mut chats = HashSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if platform == "qqbot" || (platform == "telegram" && value.starts_with('-')) {
            chats.insert(value.to_string());
        } else {
            users.insert(value.to_string());
        }
    }
    (users, chats)
}

const GATEWAY_CONFIG_DIRECT_USER_ALLOWLIST_EXTRA_KEYS: &[&str] = &[
    "allow_from",
    "allowed_user_ids",
    "allowed_senders",
    "allowed_accounts",
];
const GATEWAY_CONFIG_GROUP_USER_ALLOWLIST_EXTRA_KEYS: &[&str] =
    &["group_allow_from", "group_allowed_users"];
const GATEWAY_CONFIG_GROUP_CHAT_ALLOWLIST_EXTRA_KEYS: &[&str] = &[
    "group_allowed_chats",
    "allowed_group_chats",
    "allowed_groups",
];

fn build_gateway_dm_manager(config: &hermes_config::GatewayConfig) -> DmManager {
    build_gateway_dm_manager_with_lookup(config, |key| std::env::var(key).ok())
}

fn build_gateway_dm_manager_with_lookup<F>(
    config: &hermes_config::GatewayConfig,
    mut lookup: F,
) -> DmManager
where
    F: FnMut(&str) -> Option<String>,
{
    let mut global_users = env_list_from_lookup(&mut lookup, "GATEWAY_ALLOWED_USERS");
    if env_truthy_from_lookup(&mut lookup, "GATEWAY_ALLOW_ALL_USERS") {
        global_users.insert("*".to_string());
    }
    let has_global_allowlist = !global_users.is_empty();
    let mut dm_manager = DmManager::new(
        global_users,
        HashSet::new(),
        if has_global_allowlist {
            UnauthorizedDmBehavior::Ignore
        } else {
            UnauthorizedDmBehavior::Pair
        },
    );
    let hermes_home_dir = config
        .home_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_home);
    for (platform, platform_cfg) in config.platforms.iter().filter(|(_, p)| p.enabled) {
        let platform = canonical_gateway_platform(platform);
        let mut has_platform_allowlist = false;
        for user in &platform_cfg.allowed_users {
            let trimmed = user.trim();
            if !trimmed.is_empty() {
                has_platform_allowlist = true;
                dm_manager.authorize_user_for_platform(&platform, trimmed.to_string());
            }
        }
        for admin in &platform_cfg.admin_users {
            let trimmed = admin.trim();
            if !trimmed.is_empty() {
                has_platform_allowlist = true;
                dm_manager.add_admin_for_platform(&platform, trimmed.to_string());
            }
        }
        for key in GATEWAY_CONFIG_DIRECT_USER_ALLOWLIST_EXTRA_KEYS {
            for user in extra_string_set(platform_cfg, key) {
                has_platform_allowlist = true;
                dm_manager.authorize_user_for_platform(&platform, user);
            }
        }
        if gateway_platform_config_allows_all_users(platform_cfg) {
            has_platform_allowlist = true;
            dm_manager.authorize_user_for_platform(&platform, "*");
        }
        if let Some(env) = platform_gateway_auth_env(&platform) {
            for user in env_list_from_lookup(&mut lookup, env.allowed_users) {
                has_platform_allowlist = true;
                dm_manager.authorize_user_for_platform(&platform, user);
            }
            if env_truthy_from_lookup(&mut lookup, env.allow_all_users) {
                has_platform_allowlist = true;
                dm_manager.authorize_user_for_platform(&platform, "*");
            }
            if let Some(group_users_env) = env.group_allowed_users {
                let (users, chats) = split_group_authorization_values(
                    &platform,
                    env_list_from_lookup(&mut lookup, group_users_env),
                );
                for user in users {
                    has_platform_allowlist = true;
                    dm_manager.authorize_group_user_for_platform(&platform, user);
                }
                for chat in chats {
                    has_platform_allowlist = true;
                    dm_manager.authorize_group_chat_for_platform(&platform, chat);
                }
            }
            if let Some(group_chats_env) = env.group_allowed_chats {
                for chat in env_list_from_lookup(&mut lookup, group_chats_env) {
                    has_platform_allowlist = true;
                    dm_manager.authorize_group_chat_for_platform(&platform, chat);
                }
            }
        }
        let mut config_group_values = Vec::new();
        for key in GATEWAY_CONFIG_GROUP_USER_ALLOWLIST_EXTRA_KEYS {
            config_group_values.extend(extra_string_set(platform_cfg, key));
        }
        let (group_users, legacy_group_chats) =
            split_group_authorization_values(&platform, config_group_values);
        for user in group_users {
            has_platform_allowlist = true;
            dm_manager.authorize_group_user_for_platform(&platform, user);
        }
        for chat in legacy_group_chats {
            has_platform_allowlist = true;
            dm_manager.authorize_group_chat_for_platform(&platform, chat);
        }
        for key in GATEWAY_CONFIG_GROUP_CHAT_ALLOWLIST_EXTRA_KEYS {
            for chat in extra_string_set(platform_cfg, key) {
                has_platform_allowlist = true;
                dm_manager.authorize_group_chat_for_platform(&platform, chat);
            }
        }
        if platform == "whatsapp" {
            dm_manager.load_whatsapp_lid_mappings_from_home(&hermes_home_dir);
        }
        if let Some(behavior) =
            explicit_platform_unauthorized_dm_behavior(&platform, platform_cfg, &mut lookup)
        {
            dm_manager.set_platform_unauthorized_behavior(&platform, behavior);
        } else if has_platform_allowlist {
            dm_manager
                .set_platform_unauthorized_behavior(&platform, UnauthorizedDmBehavior::Ignore);
        }
    }
    dm_manager
}

const GATEWAY_USER_ALLOWLIST_ENV_VARS: &[&str] = &[
    "TELEGRAM_ALLOWED_USERS",
    "TELEGRAM_GROUP_ALLOWED_USERS",
    "TELEGRAM_GROUP_ALLOWED_CHATS",
    "DISCORD_ALLOWED_USERS",
    "WHATSAPP_ALLOWED_USERS",
    "SLACK_ALLOWED_USERS",
    "SIGNAL_ALLOWED_USERS",
    "SIGNAL_GROUP_ALLOWED_USERS",
    "EMAIL_ALLOWED_USERS",
    "SMS_ALLOWED_USERS",
    "MATTERMOST_ALLOWED_USERS",
    "MATRIX_ALLOWED_USERS",
    "DINGTALK_ALLOWED_USERS",
    "FEISHU_ALLOWED_USERS",
    "WECOM_ALLOWED_USERS",
    "QQ_ALLOWED_USERS",
    "QQ_GROUP_ALLOWED_USERS",
    "GATEWAY_ALLOWED_USERS",
];

const GATEWAY_ALLOW_ALL_ENV_VARS: &[&str] = &[
    "GATEWAY_ALLOW_ALL_USERS",
    "TELEGRAM_ALLOW_ALL_USERS",
    "DISCORD_ALLOW_ALL_USERS",
    "WHATSAPP_ALLOW_ALL_USERS",
    "SLACK_ALLOW_ALL_USERS",
    "SIGNAL_ALLOW_ALL_USERS",
    "EMAIL_ALLOW_ALL_USERS",
    "SMS_ALLOW_ALL_USERS",
    "MATTERMOST_ALLOW_ALL_USERS",
    "MATRIX_ALLOW_ALL_USERS",
    "DINGTALK_ALLOW_ALL_USERS",
    "FEISHU_ALLOW_ALL_USERS",
    "WECOM_ALLOW_ALL_USERS",
    "QQ_ALLOW_ALL_USERS",
];

const GATEWAY_CONFIG_USER_ALLOWLIST_EXTRA_KEYS: &[&str] = &[
    "allow_from",
    "group_allow_from",
    "group_allowed_users",
    "group_allowed_chats",
    "allowed_user_ids",
    "allowed_senders",
    "allowed_accounts",
    "allowed_group_chats",
    "allowed_groups",
];

fn env_value_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn string_list_has_non_empty(values: &[String]) -> bool {
    values.iter().any(|value| !value.trim().is_empty())
}

fn gateway_platform_config_has_allowlist(platform_cfg: &PlatformConfig) -> bool {
    string_list_has_non_empty(&platform_cfg.allowed_users)
        || string_list_has_non_empty(&platform_cfg.admin_users)
        || GATEWAY_CONFIG_USER_ALLOWLIST_EXTRA_KEYS
            .iter()
            .any(|key| !extra_string_set(platform_cfg, key).is_empty())
}

fn gateway_platform_config_allows_all_users(platform_cfg: &PlatformConfig) -> bool {
    extra_bool_loose(platform_cfg, "allow_all_users").unwrap_or(false)
}

fn gateway_config_has_allowlist_or_allow_all(config: &GatewayConfig) -> bool {
    config
        .platforms
        .values()
        .filter(|platform_cfg| platform_cfg.enabled)
        .any(|platform_cfg| {
            gateway_platform_config_has_allowlist(platform_cfg)
                || gateway_platform_config_allows_all_users(platform_cfg)
        })
}

fn gateway_allowlist_startup_would_warn_from_lookup<F>(mut lookup: F) -> bool
where
    F: FnMut(&str) -> Option<String>,
{
    let any_allowlist = GATEWAY_USER_ALLOWLIST_ENV_VARS.iter().any(|key| {
        lookup(key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    });
    let allow_all = GATEWAY_ALLOW_ALL_ENV_VARS
        .iter()
        .any(|key| lookup(key).is_some_and(|value| env_value_truthy(&value)));
    !any_allowlist && !allow_all
}

fn gateway_allowlist_startup_would_warn_with_lookup<F>(config: &GatewayConfig, lookup: F) -> bool
where
    F: FnMut(&str) -> Option<String>,
{
    !gateway_config_has_allowlist_or_allow_all(config)
        && gateway_allowlist_startup_would_warn_from_lookup(lookup)
}

fn gateway_allowlist_startup_would_warn(config: &GatewayConfig) -> bool {
    gateway_allowlist_startup_would_warn_with_lookup(config, |key| std::env::var(key).ok())
}

fn explicit_group_access_mode(platform_cfg: &PlatformConfig) -> Option<GroupAccessMode> {
    let explicit = extra_string(platform_cfg, "group_policy")
        .or_else(|| extra_string(platform_cfg, "group_access"));
    if let Some(policy) = explicit {
        match policy.trim().to_ascii_lowercase().as_str() {
            "disabled" | "deny" | "off" | "none" => return Some(GroupAccessMode::Disabled),
            "allowlist" | "restricted" | "whitelist" => return Some(GroupAccessMode::Allowlist),
            "open" | "all" | "enabled" => return Some(GroupAccessMode::Open),
            _ => {}
        }
    }
    None
}

fn parse_group_access_mode(
    platform_cfg: &PlatformConfig,
    has_group_authorization: bool,
) -> GroupAccessMode {
    if let Some(mode) = explicit_group_access_mode(platform_cfg) {
        return mode;
    }
    if has_group_authorization
        || !platform_cfg.allowed_users.is_empty()
        || !platform_cfg.admin_users.is_empty()
    {
        GroupAccessMode::Allowlist
    } else {
        GroupAccessMode::Open
    }
}

fn build_gateway_platform_access_policies(
    config: &hermes_config::GatewayConfig,
) -> std::collections::HashMap<String, PlatformAccessPolicy> {
    build_gateway_platform_access_policies_with_lookup(config, |key| std::env::var(key).ok())
}

fn build_gateway_platform_access_policies_with_lookup<F>(
    config: &hermes_config::GatewayConfig,
    mut lookup: F,
) -> std::collections::HashMap<String, PlatformAccessPolicy>
where
    F: FnMut(&str) -> Option<String>,
{
    let global_allowed_users = env_list_from_lookup(&mut lookup, "GATEWAY_ALLOWED_USERS");
    let global_allow_all = env_truthy_from_lookup(&mut lookup, "GATEWAY_ALLOW_ALL_USERS");
    let mut policies = std::collections::HashMap::new();
    for (platform, platform_cfg) in config.platforms.iter().filter(|(_, cfg)| cfg.enabled) {
        let platform = canonical_gateway_platform(platform);
        let mut allowed_users = HashSet::new();
        let mut admin_users = HashSet::new();
        let mut authorized_group_chats = HashSet::new();
        for user in &platform_cfg.allowed_users {
            let trimmed = user.trim();
            if !trimmed.is_empty() {
                allowed_users.insert(trimmed.to_string());
            }
        }
        for admin in &platform_cfg.admin_users {
            let trimmed = admin.trim();
            if !trimmed.is_empty() {
                admin_users.insert(trimmed.to_string());
            }
        }
        for key in GATEWAY_CONFIG_DIRECT_USER_ALLOWLIST_EXTRA_KEYS {
            allowed_users.extend(extra_string_set(platform_cfg, key));
        }
        allowed_users.extend(global_allowed_users.iter().cloned());
        if global_allow_all || gateway_platform_config_allows_all_users(platform_cfg) {
            allowed_users.insert("*".to_string());
        }
        if let Some(env) = platform_gateway_auth_env(&platform) {
            allowed_users.extend(env_list_from_lookup(&mut lookup, env.allowed_users));
            if env_truthy_from_lookup(&mut lookup, env.allow_all_users) {
                allowed_users.insert("*".to_string());
            }
            if let Some(group_users_env) = env.group_allowed_users {
                let (users, chats) = split_group_authorization_values(
                    &platform,
                    env_list_from_lookup(&mut lookup, group_users_env),
                );
                allowed_users.extend(users);
                authorized_group_chats.extend(chats);
            }
            if let Some(group_chats_env) = env.group_allowed_chats {
                authorized_group_chats.extend(env_list_from_lookup(&mut lookup, group_chats_env));
            }
        }
        let mut config_group_values = Vec::new();
        for key in GATEWAY_CONFIG_GROUP_USER_ALLOWLIST_EXTRA_KEYS {
            config_group_values.extend(extra_string_set(platform_cfg, key));
        }
        let (group_users, legacy_group_chats) =
            split_group_authorization_values(&platform, config_group_values);
        allowed_users.extend(group_users);
        authorized_group_chats.extend(legacy_group_chats);
        for key in GATEWAY_CONFIG_GROUP_CHAT_ALLOWLIST_EXTRA_KEYS {
            authorized_group_chats.extend(extra_string_set(platform_cfg, key));
        }

        let group_mode = parse_group_access_mode(
            platform_cfg,
            !authorized_group_chats.is_empty()
                || !allowed_users.is_empty()
                || !admin_users.is_empty(),
        );
        let has_allowlist = !allowed_users.is_empty() || !admin_users.is_empty();
        let slash_requires_allowlist = extra_bool_loose(platform_cfg, "slash_requires_allowlist")
            .or_else(|| extra_bool_loose(platform_cfg, "require_allowlist_for_slash"))
            .unwrap_or_else(|| platform == "discord" && has_allowlist);

        let mut allowed_channels = extra_string_set(platform_cfg, "allowed_channels");
        if platform == "telegram" {
            allowed_channels.extend(extra_string_set(platform_cfg, "allowed_chats"));
        }
        if platform == "dingtalk" {
            allowed_channels.extend(extra_string_set(platform_cfg, "allowed_chats"));
        }
        if platform == "matrix" {
            allowed_channels.extend(extra_string_set(platform_cfg, "allowed_rooms"));
        }
        let mut ignored_channels = extra_string_set(platform_cfg, "ignored_channels");
        if platform == "telegram" {
            ignored_channels.extend(extra_string_set(platform_cfg, "ignored_threads"));
        }

        policies.insert(
            platform.clone(),
            PlatformAccessPolicy {
                allowed_users,
                admin_users,
                allowed_channels,
                authorized_group_chats,
                ignored_channels,
                group_mode,
                slash_requires_allowlist,
                bot_sender_bypasses_allowlist: discord_allow_bots_bypasses_gateway_allowlist(
                    &platform,
                    platform_cfg,
                ),
                reactions_enabled: extra_bool_loose(platform_cfg, "reactions"),
            },
        );
    }
    policies
}

fn gateway_requirement_issues(config: &hermes_config::GatewayConfig) -> Vec<String> {
    let mut issues = Vec::new();

    let check = |enabled: bool, cond: bool| enabled && !cond;

    if let Some(p) = config.platforms.get("telegram") {
        if check(p.enabled, platform_token_or_extra(p).is_some()) {
            issues.push("telegram.enabled=true 但缺少 token".to_string());
        }
    }
    if let Some(p) = config.platforms.get("weixin") {
        let account_id = extra_string(p, "account_id").is_some();
        let token = platform_token_or_extra(p).is_some();
        if check(p.enabled, account_id && token) {
            issues.push("weixin.enabled=true 但缺少 account_id 或 token".to_string());
        }
    }
    if let Some(p) = config.platforms.get("discord") {
        if check(p.enabled, platform_token_or_extra(p).is_some()) {
            issues.push("discord.enabled=true 但缺少 token".to_string());
        }
    }
    if let Some(p) = config.platforms.get("slack") {
        if check(p.enabled, platform_token_or_extra(p).is_some()) {
            issues.push("slack.enabled=true 但缺少 token".to_string());
        }
    }
    if let Some(p) = config.platforms.get("ntfy") {
        let topic = extra_string(p, "topic").is_some() || std::env::var("NTFY_TOPIC").is_ok();
        if check(p.enabled, topic) {
            issues.push("ntfy.enabled=true but topic is missing".to_string());
        }
    }
    if let Some(p) = config
        .platforms
        .get("qqbot")
        .or_else(|| config.platforms.get("qq"))
    {
        let app_id = extra_string(p, "app_id").is_some();
        let secret = extra_string(p, "client_secret").is_some();
        if check(p.enabled, app_id && secret) {
            issues.push("qqbot.enabled=true 但缺少 app_id 或 client_secret".to_string());
        }
    }
    if let Some(p) = config.platforms.get("wecom_callback") {
        let ready = extra_string(p, "corp_id").is_some()
            && extra_string(p, "corp_secret").is_some()
            && extra_string(p, "agent_id").is_some()
            && platform_token_or_extra(p)
                .or_else(|| extra_string(p, "token"))
                .is_some()
            && extra_string(p, "encoding_aes_key").is_some();
        if check(p.enabled, ready) {
            issues.push(
                "wecom_callback.enabled=true 但缺少 corp_id/corp_secret/agent_id/token/encoding_aes_key"
                    .to_string(),
            );
        }
    }

    issues
}

fn build_api_server_config(platform_cfg: &PlatformConfig) -> ApiServerConfig {
    ApiServerConfig {
        host: extra_string(platform_cfg, "host").unwrap_or_else(|| "127.0.0.1".to_string()),
        port: extra_u16(platform_cfg, "port", 8090),
        auth_token: platform_token_or_extra(platform_cfg)
            .or_else(|| extra_string(platform_cfg, "auth_token")),
    }
}

fn build_webhook_config(platform_cfg: &PlatformConfig, secret: String) -> WebhookConfig {
    let routes = platform_cfg
        .extra
        .get("routes")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    WebhookConfig {
        host: extra_string(platform_cfg, "host").unwrap_or_else(|| "0.0.0.0".to_string()),
        port: extra_u16(platform_cfg, "port", 9000),
        path: extra_string(platform_cfg, "path").unwrap_or_else(|| "/webhook".to_string()),
        secret,
        rate_limit: platform_cfg
            .extra
            .get("rate_limit")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(30),
        max_body_bytes: platform_cfg
            .extra
            .get("max_body_bytes")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(1_048_576),
        routes,
    }
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
            message_id: Some(req.request_id.clone()),
            is_dm: true,
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

async fn register_gateway_adapters(
    config: &hermes_config::GatewayConfig,
    gateway: Arc<Gateway>,
    sidecar_tasks: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<(), AgentError> {
    if let Some(platform_cfg) = config.platforms.get("telegram") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
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
            let token_missing = platform_token_or_extra(platform_cfg).is_none();
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
                        let adapter = Arc::new(adapter);
                        let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                        adapter.set_inbound_sender(tx).await;
                        gateway.register_adapter("weixin", adapter).await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            run_gateway_incoming_loop(gw_clone, rx, "weixin").await;
                        }));
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

    if let Some(platform_cfg) = config.platforms.get("discord") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let discord_cfg = DiscordConfig {
                    token,
                    application_id: extra_string(platform_cfg, "application_id"),
                    proxy: Default::default(),
                    require_mention: platform_cfg.require_mention.unwrap_or(false),
                    intents: platform_cfg
                        .extra
                        .get("intents")
                        .and_then(|v| v.as_u64())
                        .unwrap_or((1 << 0) | (1 << 9) | (1 << 15)),
                    reply_to_mode: discord_reply_to_mode_string(platform_cfg)
                        .unwrap_or_else(|| "first".to_string()),
                    channel_controls: DiscordChannelControls::from_extra(&platform_cfg.extra),
                    channel_skill_bindings: DiscordChannelSkillBinding::list_from_json(
                        platform_cfg.extra.get("channel_skill_bindings"),
                    ),
                };
                match DiscordAdapter::new(discord_cfg) {
                    Ok(adapter) => gateway.register_adapter("discord", Arc::new(adapter)).await,
                    Err(e) => println!("Discord enabled but failed to initialize: {}", e),
                }
            } else {
                println!("Discord is enabled but token is missing; skipping discord adapter.");
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("slack") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let slack_cfg = SlackConfig {
                    token,
                    app_token: extra_string(platform_cfg, "app_token"),
                    socket_mode: extra_bool(platform_cfg, "socket_mode", false),
                    reactions: extra_bool(platform_cfg, "reactions", true),
                    proxy: Default::default(),
                };
                match SlackAdapter::new(slack_cfg) {
                    Ok(adapter) => gateway.register_adapter("slack", Arc::new(adapter)).await,
                    Err(e) => println!("Slack enabled but failed to initialize: {}", e),
                }
            } else {
                println!("Slack is enabled but token is missing; skipping slack adapter.");
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("matrix") {
        if platform_cfg.enabled {
            let homeserver_url = extra_string(platform_cfg, "homeserver_url")
                .or_else(|| extra_string(platform_cfg, "homeserver"))
                .unwrap_or_default();
            let user_id = extra_string(platform_cfg, "user_id").unwrap_or_default();
            let access_token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "access_token"))
                .unwrap_or_default();
            if homeserver_url.is_empty() || user_id.is_empty() || access_token.is_empty() {
                println!(
                    "Matrix is enabled but homeserver_url/user_id/access_token is incomplete; skipping matrix adapter."
                );
            } else {
                let matrix_cfg = MatrixConfig {
                    homeserver_url,
                    user_id,
                    access_token,
                    room_id: matrix_home_room_for_platform(platform_cfg),
                    proxy: Default::default(),
                };
                match MatrixAdapter::new(matrix_cfg) {
                    Ok(adapter) => gateway.register_adapter("matrix", Arc::new(adapter)).await,
                    Err(e) => println!("Matrix enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("mattermost") {
        if platform_cfg.enabled {
            let token = platform_token_or_extra(platform_cfg).unwrap_or_default();
            let server_url = extra_string(platform_cfg, "server_url")
                .or_else(|| extra_string(platform_cfg, "url"))
                .unwrap_or_default();
            if token.is_empty() || server_url.is_empty() {
                println!(
                    "Mattermost is enabled but server_url/token is missing; skipping mattermost adapter."
                );
            } else {
                let mm_cfg = MattermostConfig {
                    server_url,
                    token,
                    team_id: extra_string(platform_cfg, "team_id"),
                    proxy: Default::default(),
                };
                match MattermostAdapter::new(mm_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("mattermost", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("Mattermost enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("signal") {
        if platform_cfg.enabled {
            let phone_number = extra_string(platform_cfg, "phone_number")
                .or_else(|| extra_string(platform_cfg, "account"))
                .unwrap_or_default();
            if phone_number.is_empty() {
                println!("Signal is enabled but phone_number is missing; skipping signal adapter.");
            } else {
                let signal_cfg = SignalConfig {
                    phone_number,
                    api_url: extra_string(platform_cfg, "api_url")
                        .unwrap_or_else(|| "http://localhost:8080".to_string()),
                    proxy: Default::default(),
                };
                match SignalAdapter::new(signal_cfg) {
                    Ok(adapter) => gateway.register_adapter("signal", Arc::new(adapter)).await,
                    Err(e) => println!("Signal enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("whatsapp") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let wa_cfg = WhatsAppConfig {
                    token,
                    phone_number_id: extra_string(platform_cfg, "phone_number_id"),
                    business_account_id: extra_string(platform_cfg, "business_account_id"),
                    verify_token: extra_string(platform_cfg, "verify_token"),
                    reply_prefix: platform_cfg
                        .extra
                        .get("reply_prefix")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    require_mention: extra_bool(platform_cfg, "require_mention", false),
                    mention_patterns: extra_string_vec(platform_cfg, "mention_patterns"),
                    free_response_chats: extra_string_vec(platform_cfg, "free_response_chats"),
                    dm_policy: extra_string(platform_cfg, "dm_policy")
                        .unwrap_or_else(|| "open".to_string()),
                    allow_from: extra_string_vec(platform_cfg, "allow_from"),
                    group_policy: extra_string(platform_cfg, "group_policy")
                        .unwrap_or_else(|| "open".to_string()),
                    group_allow_from: extra_string_vec(platform_cfg, "group_allow_from"),
                    proxy: Default::default(),
                };
                match WhatsAppAdapter::new(wa_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("whatsapp", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("WhatsApp enabled but failed to initialize: {}", e),
                }
            } else {
                println!("WhatsApp is enabled but token is missing; skipping whatsapp adapter.");
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("dingtalk") {
        if platform_cfg.enabled {
            let ding_cfg = DingTalkConfig::from_platform_config(platform_cfg);
            match DingTalkAdapter::new(ding_cfg) {
                Ok(adapter) => {
                    let adapter = Arc::new(adapter);
                    let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                    adapter.set_inbound_sender(tx).await;
                    gateway.register_adapter("dingtalk", adapter).await;
                    let gw_clone = gateway.clone();
                    sidecar_tasks.push(tokio::spawn(async move {
                        run_gateway_incoming_loop(gw_clone, rx, "dingtalk").await;
                    }));
                }
                Err(e) => println!("DingTalk enabled but failed to initialize: {}", e),
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("feishu") {
        if platform_cfg.enabled {
            let app_id = extra_string(platform_cfg, "app_id").unwrap_or_default();
            let app_secret = extra_string(platform_cfg, "app_secret").unwrap_or_default();
            if app_id.is_empty() || app_secret.is_empty() {
                println!(
                    "Feishu is enabled but app_id/app_secret is missing; skipping feishu adapter."
                );
            } else {
                let feishu_cfg = FeishuConfig {
                    app_id,
                    app_secret,
                    verification_token: extra_string(platform_cfg, "verification_token"),
                    encrypt_key: extra_string(platform_cfg, "encrypt_key"),
                    proxy: Default::default(),
                };
                match FeishuAdapter::new(feishu_cfg) {
                    Ok(adapter) => gateway.register_adapter("feishu", Arc::new(adapter)).await,
                    Err(e) => println!("Feishu enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("wecom") {
        if platform_cfg.enabled {
            let corp_id = extra_string(platform_cfg, "corp_id").unwrap_or_default();
            let agent_id = extra_string(platform_cfg, "agent_id").unwrap_or_default();
            let secret = extra_string(platform_cfg, "secret").unwrap_or_default();
            if corp_id.is_empty() || agent_id.is_empty() || secret.is_empty() {
                println!(
                    "WeCom is enabled but corp_id/agent_id/secret is missing; skipping wecom adapter."
                );
            } else {
                let wecom_cfg = WeComConfig {
                    corp_id,
                    agent_id,
                    secret,
                    proxy: Default::default(),
                };
                match WeComAdapter::new(wecom_cfg) {
                    Ok(adapter) => gateway.register_adapter("wecom", Arc::new(adapter)).await,
                    Err(e) => println!("WeCom enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("wecom_callback") {
        if platform_cfg.enabled {
            let corp_id = extra_string(platform_cfg, "corp_id").unwrap_or_default();
            let corp_secret = extra_string(platform_cfg, "corp_secret").unwrap_or_default();
            let agent_id = extra_string(platform_cfg, "agent_id").unwrap_or_default();
            let token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "token"))
                .unwrap_or_default();
            let encoding_aes_key =
                extra_string(platform_cfg, "encoding_aes_key").unwrap_or_default();
            if corp_id.is_empty()
                || corp_secret.is_empty()
                || agent_id.is_empty()
                || token.is_empty()
                || encoding_aes_key.is_empty()
            {
                println!(
                    "WeCom callback is enabled but corp_id/corp_secret/agent_id/token/encoding_aes_key is incomplete; skipping wecom_callback adapter."
                );
            } else {
                let app = WeComCallbackApp {
                    name: extra_string(platform_cfg, "app_name")
                        .unwrap_or_else(|| "default".to_string()),
                    corp_id,
                    corp_secret,
                    agent_id,
                    token,
                    encoding_aes_key,
                };
                let wecom_cb_cfg = WeComCallbackConfig {
                    host: extra_string(platform_cfg, "host")
                        .unwrap_or_else(|| "0.0.0.0".to_string()),
                    port: extra_u16(platform_cfg, "port", 8645),
                    path: extra_string(platform_cfg, "path")
                        .unwrap_or_else(|| "/wecom/callback".to_string()),
                    apps: vec![app],
                    proxy: Default::default(),
                };
                match WeComCallbackAdapter::new(wecom_cb_cfg) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        let (tx, mut rx) =
                            tokio::sync::mpsc::channel::<GatewayIncomingMessage>(128);
                        adapter.set_inbound_sender(tx).await;
                        gateway
                            .register_adapter("wecom_callback", adapter.clone())
                            .await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            while let Some(incoming) = rx.recv().await {
                                if let Err(err) = gw_clone.route_message(&incoming).await {
                                    tracing::warn!(
                                        "Failed to route wecom_callback message: {}",
                                        err
                                    );
                                }
                            }
                        }));
                    }
                    Err(e) => println!("WeCom callback enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config
        .platforms
        .get("qqbot")
        .or_else(|| config.platforms.get("qq"))
    {
        if platform_cfg.enabled {
            let app_id = extra_string(platform_cfg, "app_id").unwrap_or_default();
            let client_secret = extra_string(platform_cfg, "client_secret").unwrap_or_default();
            if app_id.is_empty() || client_secret.is_empty() {
                println!(
                    "QQBot is enabled but app_id/client_secret is missing; skipping qqbot adapter."
                );
            } else {
                let qq_cfg = QqBotConfig {
                    app_id,
                    client_secret,
                    markdown_support: extra_bool(platform_cfg, "markdown_support", true),
                    proxy: Default::default(),
                };
                match QqBotAdapter::new(qq_cfg) {
                    Ok(adapter) => gateway.register_adapter("qqbot", Arc::new(adapter)).await,
                    Err(e) => println!("QQBot enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("bluebubbles") {
        if platform_cfg.enabled {
            let server_url = extra_string(platform_cfg, "server_url").unwrap_or_default();
            let password = extra_string(platform_cfg, "password").unwrap_or_default();
            if server_url.is_empty() || password.is_empty() {
                println!(
                    "BlueBubbles is enabled but server_url/password is missing; skipping bluebubbles adapter."
                );
            } else {
                let bb_cfg = BlueBubblesConfig {
                    server_url,
                    password,
                    proxy: Default::default(),
                };
                match BlueBubblesAdapter::new(bb_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("bluebubbles", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("BlueBubbles enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("email") {
        if platform_cfg.enabled {
            let imap_host = extra_string(platform_cfg, "imap_host").unwrap_or_default();
            let smtp_host = extra_string(platform_cfg, "smtp_host").unwrap_or_default();
            let username = extra_string(platform_cfg, "username").unwrap_or_default();
            let password = extra_string(platform_cfg, "password").unwrap_or_default();
            if imap_host.is_empty()
                || smtp_host.is_empty()
                || username.is_empty()
                || password.is_empty()
            {
                println!(
                    "Email is enabled but imap/smtp/username/password is incomplete; skipping email adapter."
                );
            } else {
                let email_cfg = EmailConfig {
                    imap_host,
                    imap_port: extra_u16(platform_cfg, "imap_port", 993),
                    smtp_host,
                    smtp_port: extra_u16(platform_cfg, "smtp_port", 587),
                    username,
                    password,
                    poll_interval_secs: platform_cfg
                        .extra
                        .get("poll_interval_secs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(60),
                    proxy: Default::default(),
                };
                match EmailAdapter::new(email_cfg) {
                    Ok(adapter) => gateway.register_adapter("email", Arc::new(adapter)).await,
                    Err(e) => println!("Email enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("sms") {
        if platform_cfg.enabled {
            let account_sid = extra_string(platform_cfg, "account_sid").unwrap_or_default();
            let auth_token = extra_string(platform_cfg, "auth_token").unwrap_or_default();
            let from_number = extra_string(platform_cfg, "from_number").unwrap_or_default();
            if account_sid.is_empty() || auth_token.is_empty() || from_number.is_empty() {
                println!(
                    "SMS is enabled but account_sid/auth_token/from_number is incomplete; skipping sms adapter."
                );
            } else {
                let sms_cfg = SmsConfig {
                    provider: extra_string(platform_cfg, "provider")
                        .unwrap_or_else(|| "twilio".to_string()),
                    account_sid,
                    auth_token,
                    from_number,
                    proxy: Default::default(),
                };
                match SmsAdapter::new(sms_cfg) {
                    Ok(adapter) => gateway.register_adapter("sms", Arc::new(adapter)).await,
                    Err(e) => println!("SMS enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("homeassistant") {
        if platform_cfg.enabled {
            let base_url = extra_string(platform_cfg, "base_url").unwrap_or_default();
            let long_lived_token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "long_lived_token"))
                .unwrap_or_default();
            if base_url.is_empty() || long_lived_token.is_empty() {
                println!(
                    "HomeAssistant is enabled but base_url/token is missing; skipping homeassistant adapter."
                );
            } else {
                let ha_cfg = HomeAssistantConfig {
                    base_url,
                    long_lived_token,
                    webhook_id: extra_string(platform_cfg, "webhook_id"),
                    proxy: Default::default(),
                };
                match HomeAssistantAdapter::new(ha_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("homeassistant", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("HomeAssistant enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("ntfy") {
        if platform_cfg.enabled {
            let ntfy_cfg = NtfyConfig::from_platform_config(platform_cfg);
            match NtfyAdapter::new(ntfy_cfg) {
                Ok(adapter) => {
                    let adapter = Arc::new(adapter);
                    let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                    adapter.set_inbound_sender(tx).await;
                    gateway.register_adapter("ntfy", adapter).await;
                    let gw_clone = gateway.clone();
                    sidecar_tasks.push(tokio::spawn(async move {
                        run_gateway_incoming_loop(gw_clone, rx, "ntfy").await;
                    }));
                }
                Err(e) => println!("ntfy enabled but failed to initialize: {}", e),
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("webhook") {
        if platform_cfg.enabled {
            let secret = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "secret"))
                .unwrap_or_default();
            if secret.is_empty() {
                println!("Webhook is enabled but secret is missing; skipping webhook adapter.");
            } else {
                let wh_cfg = build_webhook_config(platform_cfg, secret);
                let adapter = Arc::new(WebhookAdapter::new(wh_cfg));
                let (tx, rx) = mpsc::channel::<WebhookPayload>(512);
                adapter.set_inbound_sender(tx).await;
                gateway.register_adapter("webhook", adapter).await;
                let gw_clone = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    run_webhook_inbound_loop(gw_clone, rx).await;
                }));
            }
        }
    }

    if let Some(platform_cfg) = config
        .platforms
        .get("api_server")
        .or_else(|| config.platforms.get("api-server"))
    {
        if platform_cfg.enabled {
            let api_cfg = build_api_server_config(platform_cfg);
            let adapter = Arc::new(ApiServerAdapter::new(api_cfg.clone()));
            let (tx, rx) = mpsc::channel::<ApiInboundRequest>(256);
            adapter.set_inbound_sender(tx).await;
            gateway.register_adapter("api_server", adapter).await;
            let gw_clone = gateway.clone();
            sidecar_tasks.push(tokio::spawn(async move {
                run_api_server_inbound_loop(gw_clone, rx).await;
            }));
            println!(
                "API server adapter enabled on {}:{}",
                api_cfg.host, api_cfg.port
            );
        }
    }

    Ok(())
}

fn telegram_should_batch_text(msg: &TelegramIncomingMessage) -> bool {
    msg.text
        .as_deref()
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false)
        && !msg.is_voice
        && !msg.is_photo
        && !msg.is_sticker
        && !msg.is_document
        && msg.callback_query_id.is_none()
        && msg.callback_data.is_none()
}

fn telegram_gateway_message(msg: TelegramIncomingMessage) -> GatewayIncomingMessage {
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

    let chat_id = match msg.message_thread_id {
        Some(thread_id) if msg.is_group && thread_id != 0 => {
            format!("{}:{}", msg.chat_id, thread_id)
        }
        _ => msg.chat_id.to_string(),
    };

    GatewayIncomingMessage {
        platform: "telegram".to_string(),
        chat_id,
        user_id,
        text,
        message_id: Some(msg.message_id.to_string()),
        is_dm: msg.chat_id > 0,
    }
}

async fn route_telegram_message(gateway: &Gateway, msg: TelegramIncomingMessage) {
    let incoming = telegram_gateway_message(msg);
    if let Err(err) = gateway.route_message(&incoming).await {
        tracing::warn!("Failed to route telegram message: {}", err);
    }
}

async fn run_telegram_poll_loop(gateway: Arc<Gateway>, adapter: Arc<TelegramAdapter>) {
    if adapter.config().polling {
        if let Err(err) = adapter.delete_webhook(false).await {
            tracing::warn!("Telegram deleteWebhook before polling failed: {}", err);
        }
    }

    let batch_delay = std::time::Duration::from_millis(adapter.config().text_batch_delay_ms);
    let mut text_batcher = TelegramTextBatcher::new(batch_delay);

    loop {
        if !adapter.is_running() {
            break;
        }

        for msg in text_batcher.drain_ready() {
            route_telegram_message(&gateway, msg).await;
        }

        match adapter.get_updates().await {
            Ok(updates) => {
                for update in updates {
                    if !adapter.should_process_update(&update) {
                        continue;
                    }
                    let Some(msg) = TelegramAdapter::parse_update(&update) else {
                        continue;
                    };

                    if let (Some(callback_id), Some(callback_data)) =
                        (&msg.callback_query_id, &msg.callback_data)
                    {
                        if callback_data.starts_with("approval:") {
                            if let Err(err) = adapter
                                .handle_approval_callback(callback_id, callback_data)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to handle telegram approval callback: {}",
                                    err
                                );
                            }
                            continue;
                        }
                    }

                    if batch_delay.is_zero() || !telegram_should_batch_text(&msg) {
                        route_telegram_message(&gateway, msg).await;
                    } else {
                        text_batcher.enqueue(msg);
                    }
                }
                if text_batcher.pending_len() > 0 && !batch_delay.is_zero() {
                    tokio::time::sleep(batch_delay).await;
                    for msg in text_batcher.drain_ready() {
                        route_telegram_message(&gateway, msg).await;
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
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
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
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "api-server" => "api_server".to_string(),
        "home-assistant" => "homeassistant".to_string(),
        "wecom-callback" => "wecom_callback".to_string(),
        "mm" => "mattermost".to_string(),
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
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
        "ntfy" => Some("ntfy"),
        "webhook" => Some("webhook"),
        "api_server" => Some("api_server"),
        _ => None,
    }
}

fn normalize_secret_provider(provider: &str) -> String {
    let p = provider.trim().to_ascii_lowercase();
    match p.as_str() {
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "codex" => "openai-codex".to_string(),
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
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
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon" => "bedrock".to_string(),
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
        "nous-api" => vec![
            "nous-api".to_string(),
            "nous_api".to_string(),
            "nousapi".to_string(),
            "nous-portal-api".to_string(),
        ],
        "copilot" => vec![
            "copilot".to_string(),
            "github-copilot".to_string(),
            "github-models".to_string(),
        ],
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
        "gmi" => vec![
            "gmi".to_string(),
            "gmi-cloud".to_string(),
            "gmicloud".to_string(),
        ],
        "arcee" => vec![
            "arcee".to_string(),
            "arcee-ai".to_string(),
            "arceeai".to_string(),
        ],
        "xiaomi" => vec![
            "xiaomi".to_string(),
            "mimo".to_string(),
            "xiaomi-mimo".to_string(),
        ],
        "tencent-tokenhub" => vec![
            "tencent-tokenhub".to_string(),
            "tencent".to_string(),
            "tokenhub".to_string(),
            "tencent-cloud".to_string(),
            "tencentmaas".to_string(),
        ],
        "bedrock" => vec![
            "bedrock".to_string(),
            "aws".to_string(),
            "aws-bedrock".to_string(),
            "amazon-bedrock".to_string(),
            "amazon".to_string(),
        ],
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
    let raw_provider = provider.trim().to_ascii_lowercase();
    match raw_provider.as_str() {
        "kimi-coding" => return Some("KIMI_CODING_API_KEY"),
        "moonshot" | "kimi" => return Some("KIMI_API_KEY"),
        _ => {}
    }

    match normalize_secret_provider(provider).as_str() {
        "openai" => Some("HERMES_OPENAI_API_KEY"),
        "openai-codex" => Some("HERMES_OPENAI_CODEX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "bedrock" => None,
        "google-gemini-cli" => Some("HERMES_GEMINI_OAUTH_API_KEY"),
        "gemini" => Some("GOOGLE_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "qwen" | "alibaba" => Some("DASHSCOPE_API_KEY"),
        "alibaba-coding-plan" => Some("ALIBABA_CODING_PLAN_API_KEY"),
        "qwen-oauth" => Some("HERMES_QWEN_OAUTH_API_KEY"),
        "kimi-coding" => Some("KIMI_CODING_API_KEY"),
        "kimi-coding-cn" => Some("KIMI_CN_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "minimax-cn" => Some("MINIMAX_CN_API_KEY"),
        "stepfun" => Some("STEPFUN_API_KEY"),
        "nous" | "nous-api" => Some("NOUS_API_KEY"),
        "copilot" => Some("COPILOT_GITHUB_TOKEN"),
        "ai-gateway" => Some("AI_GATEWAY_API_KEY"),
        "arcee" => Some("ARCEEAI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "huggingface" => Some("HF_TOKEN"),
        "gmi" => Some("GMI_API_KEY"),
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
        "tencent-tokenhub" => Some("TOKENHUB_API_KEY"),
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
        std::env::set_var("NOUS_API_KEY", token);
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
                std::env::set_var("NOUS_API_KEY", creds.api_key.clone());
                if !creds.base_url.trim().is_empty() {
                    std::env::set_var("NOUS_INFERENCE_BASE_URL", creds.base_url.clone());
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
        ("COPILOT_GITHUB_TOKEN", "copilot"),
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
        ("TOKENHUB_API_KEY", "tencent-tokenhub"),
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
                        std::env::set_var(env_var, secret);
                    }
                }
            }
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
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
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
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
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
            "Unknown portal action '{}'. Use `hermes-ultra portal` for setup or `hermes-ultra portal info` for status.",
            other
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
                            "OAuth flow is unavailable for provider '{}'; falling back to API key/manual token login.",
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
                println!("Ensure COPILOT_GITHUB_TOKEN is set for the agent (see printed instructions above).");
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
    let value = raw.trim().to_ascii_lowercase();
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
    Some(hermes_cron::DeliverConfig {
        target,
        platform: None,
    })
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
    workdir: Option<String>,
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
            if let Some(workdir) = workdir {
                job.workdir = Some(workdir);
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
                job.next_run = None;
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
            if let Some(workdir) = workdir {
                if workdir.trim().is_empty() {
                    job.workdir = None;
                } else {
                    job.workdir = Some(workdir);
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
    let requested = session.as_deref();
    let payload = load_resume_payload(&cli, requested)?;
    let out = output.map(PathBuf::from).unwrap_or_else(|| {
        home.join("sessions").join("saved").join(format!(
            "hermes_conversation_{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
        ))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create dump output directory {}: {}",
                parent.display(),
                e
            ))
        })?;
    }
    let system_prompt = payload
        .system_prompt
        .clone()
        .or_else(|| leading_system_prompt_for_persist(&payload.messages));
    let payload = serde_json::json!({
        "session_id": payload.session_id,
        "resolved_id": payload.resolved_id,
        "source_path": payload.source_path,
        "model": payload.model,
        "personality": payload.personality,
        "system_prompt": system_prompt,
        "session_start": payload.session_start,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "messages": payload.messages,
    });
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    )
    .map_err(|e| AgentError::Io(format!("Failed to write dump: {}", e)))?;
    println!("Wrote dump to {}", out.display());
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
const SETUP_KIMI_CODING_ENV_KEYS: &[&str] =
    &["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"];
const SETUP_KIMI_CODING_CN_ENV_KEYS: &[&str] = &["KIMI_CN_API_KEY"];
const SETUP_MINIMAX_ENV_KEYS: &[&str] = &["MINIMAX_API_KEY"];
const SETUP_MINIMAX_CN_ENV_KEYS: &[&str] = &["MINIMAX_CN_API_KEY"];
const SETUP_NOVITA_ENV_KEYS: &[&str] = &["NOVITA_API_KEY"];
const SETUP_STEPFUN_ENV_KEYS: &[&str] = &["HERMES_STEPFUN_API_KEY", "STEPFUN_API_KEY"];
const SETUP_COPILOT_ENV_KEYS: &[&str] = &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
const SETUP_AI_GATEWAY_ENV_KEYS: &[&str] = &["AI_GATEWAY_API_KEY"];
const SETUP_ARCEE_ENV_KEYS: &[&str] = &["ARCEEAI_API_KEY", "ARCEE_API_KEY"];
const SETUP_DEEPSEEK_ENV_KEYS: &[&str] = &["DEEPSEEK_API_KEY"];
const SETUP_HUGGINGFACE_ENV_KEYS: &[&str] = &["HF_TOKEN"];
const SETUP_GMI_ENV_KEYS: &[&str] = &["GMI_API_KEY"];
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
const SETUP_TENCENT_TOKENHUB_ENV_KEYS: &[&str] = &["TOKENHUB_API_KEY"];
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
        provider: "nous-api",
        model: "nous-api:openai/gpt-5.5-pro",
        label: "Nous Portal API key",
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
        provider: "bedrock",
        model: "bedrock:anthropic.claude-sonnet-4-6",
        label: "AWS Bedrock",
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
        label: "Kimi Code API",
    },
    SetupModelOption {
        provider: "kimi-coding-cn",
        model: "kimi-coding-cn:kimi-k2.6",
        label: "Kimi Coding China",
    },
    SetupModelOption {
        provider: "novita",
        model: "novita:deepseek/deepseek-v3-0324",
        label: "NovitaAI",
    },
    SetupModelOption {
        provider: "stepfun",
        model: "stepfun:step-3.5-flash",
        label: "StepFun Step Plan",
    },
    SetupModelOption {
        provider: "minimax",
        model: "minimax:MiniMax-M3",
        label: "MiniMax",
    },
    SetupModelOption {
        provider: "minimax-cn",
        model: "minimax-cn:MiniMax-M3",
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
        provider: "gmi",
        model: "gmi:gpt-oss-120b",
        label: "GMI Cloud",
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
        provider: "tencent-tokenhub",
        model: "tencent-tokenhub:hy3-preview",
        label: "Tencent TokenHub",
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

fn acp_action_from_flags(
    action: Option<String>,
    check: bool,
    setup: bool,
    setup_browser: bool,
    version: bool,
) -> Option<String> {
    if version {
        Some("version".to_string())
    } else if check {
        Some("check".to_string())
    } else if setup {
        Some("setup".to_string())
    } else if setup_browser {
        Some("setup-browser".to_string())
    } else {
        action
    }
}

fn acp_setup_browser_answer_is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
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

    if matches!(selected_provider, "nous" | "nous-api") {
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
        "nous-api" => "Nous Portal API",
        "qwen" => "Alibaba DashScope",
        "alibaba" => "Alibaba Cloud DashScope",
        "qwen-oauth" => "Qwen OAuth",
        "alibaba-coding-plan" => "Alibaba Coding Plan",
        "deepseek" => "DeepSeek",
        "kimi-coding" => "Kimi Coding",
        "kimi-coding-cn" => "Kimi Coding CN",
        "minimax" => "MiniMax",
        "minimax-cn" => "MiniMax CN",
        "novita" => "NovitaAI",
        "stepfun" => "StepFun",
        "nous" => "Nous",
        "ai-gateway" => "Vercel AI Gateway",
        "arcee" => "Arcee",
        "bedrock" => "AWS Bedrock",
        "copilot" => "GitHub Copilot",
        "huggingface" => "Hugging Face",
        "kilocode" => "KiloCode",
        "gmi" => "GMI Cloud",
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
        "tencent-tokenhub" => "Tencent TokenHub",
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
        "novita" => SETUP_NOVITA_ENV_KEYS,
        "stepfun" => SETUP_STEPFUN_ENV_KEYS,
        "nous" | "nous-api" => SETUP_NOUS_ENV_KEYS,
        "ai-gateway" => SETUP_AI_GATEWAY_ENV_KEYS,
        "arcee" => SETUP_ARCEE_ENV_KEYS,
        "bedrock" => SETUP_BEDROCK_ENV_KEYS,
        "copilot" => SETUP_COPILOT_ENV_KEYS,
        "huggingface" => SETUP_HUGGINGFACE_ENV_KEYS,
        "kilocode" => SETUP_KILOCODE_ENV_KEYS,
        "gmi" => SETUP_GMI_ENV_KEYS,
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
        "tencent-tokenhub" => SETUP_TENCENT_TOKENHUB_ENV_KEYS,
        "zai" => SETUP_ZAI_ENV_KEYS,
        _ => &[],
    }
}

fn setup_provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "openai-codex" => Some("https://chatgpt.com/backend-api/codex"),
        "nous-api" => Some(DEFAULT_NOUS_INFERENCE_URL),
        "google-gemini-cli" => Some("cloudcode-pa://google"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta"),
        "qwen" | "alibaba" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        "alibaba-coding-plan" => Some("https://coding-intl.dashscope.aliyuncs.com/v1"),
        "deepseek" => Some("https://api.deepseek.com/v1"),
        "kimi-coding" => Some(provider_profiles::KIMI_CODE_BASE_URL),
        "kimi-coding-cn" => Some(provider_profiles::KIMI_CN_BASE_URL),
        "minimax-cn" => Some("https://api.minimaxi.com/anthropic"),
        "novita" => Some("https://api.novita.ai/openai/v1"),
        "stepfun" => Some("https://api.stepfun.ai/step_plan/v1"),
        "ai-gateway" => Some("https://ai-gateway.vercel.sh/v1"),
        "arcee" => Some("https://api.arcee.ai/api/v1"),
        "copilot" => Some("https://api.githubcopilot.com"),
        "huggingface" => Some("https://router.huggingface.co/v1"),
        "kilocode" => Some("https://api.kilo.ai/api/gateway"),
        "gmi" => Some("https://api.gmi-serving.com/v1"),
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
        "tencent-tokenhub" => Some("https://tokenhub.tencentmaas.com/v1"),
        "zai" => Some("https://api.z.ai/api/paas/v4"),
        _ => None,
    }
}

fn setup_provider_requires_api_key(provider: &str) -> bool {
    !matches!(
        provider,
        "bedrock" | "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi"
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
    out.push_str(&format!(
        "# Imported by `hermes-ultra setup` from {label}\n"
    ));
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

/// Handle `hermes setup`.
async fn run_setup(cli: Cli) -> Result<(), AgentError> {
    use std::io::{self, BufRead, Write};

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
    let mut reader = stdin.lock();

    // 2. Optional import from legacy Python/OpenClaw .env files
    maybe_import_legacy_env(&mut reader, &env_path)?;

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
                "{:<22} {:<18} {} | default {}",
                setup_provider_display(option.provider),
                format!("({auth_label})"),
                provider_picker_description(option.provider),
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
        let mut answer = String::new();
        reader.read_line(&mut answer).ok();
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
            let mut answer = String::new();
            reader.read_line(&mut answer).ok();
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
            reader.read_line(&mut api_key).ok();
            api_key = api_key.trim().to_string();
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
        reader.read_line(&mut api_key).ok();
        api_key = api_key.trim().to_string();
    }

    if !api_key.is_empty() {
        print!(
            "Store {} key in encrypted vault (recommended) [Y/n]: ",
            selected_provider_label
        );
        io::stdout().flush().ok();
        let mut answer = String::new();
        reader.read_line(&mut answer).ok();
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
    let suggested_limit = if matches!(selected_provider.as_str(), "nous" | "nous-api") {
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
        let mut model_override = String::new();
        reader.read_line(&mut model_override).ok();
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
        let model_title = if matches!(selected_provider.as_str(), "nous" | "nous-api") {
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
            let mut model_override = String::new();
            reader.read_line(&mut model_override).ok();
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
        let mut personality = String::new();
        reader.read_line(&mut personality).ok();
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
        let mut answer = String::new();
        reader.read_line(&mut answer).ok();
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

    drop(reader);
    if full_setup && prompt_yes_no("\nConfigure optional setup sections now?", true).await? {
        run_optional_setup_sections(&cli, &disk).await?;
    } else if !full_setup {
        println!("Skipped optional setup sections (quick setup mode).");
    }

    println!(
        "\nSetup complete! Run `hermes-ultra` (or `hermes-agent-ultra`) to start an interactive session."
    );
    println!(
        "Run `hermes-ultra doctor` (or `hermes-agent-ultra doctor`) to check system requirements."
    );
    Ok(())
}

/// Handle `hermes doctor`.
fn build_elite_doctor_diagnostics(cli: &Cli) -> serde_json::Value {
    let provenance_path = provenance_key_path_for_cli(cli);
    let provenance_exists = provenance_path.exists();
    let provenance_key_id = if provenance_exists {
        load_or_create_provenance_key(cli, false).ok().map(|key| {
            let digest = Sha256::digest(&key);
            let full = hex::encode(digest);
            full.chars().take(16).collect::<String>()
        })
    } else {
        None
    };

    let route_path = route_learning_state_path_for_cli(cli);
    let route_state = load_route_learning_state_for_cli(&route_path).ok();
    let route_entries = route_state
        .as_ref()
        .map(|state| state.entries.len())
        .unwrap_or(0usize);
    let route_health_path = route_health_state_path_for_cli(cli);
    let route_health_summary = std::fs::read_to_string(&route_health_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("summary").cloned());

    let policy_counters_path = default_tool_policy_counters_path();
    let policy_counters = load_tool_policy_counters(&policy_counters_path).unwrap_or_default();
    let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "enforce".to_string());
    let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "relaxed".to_string());

    let elite_gate_script = std::env::var("HERMES_ELITE_GATE_CMD")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "python3 scripts/run-elite-sync-gate.py".to_string());
    let gate_available = {
        let script_path = std::env::current_dir()
            .ok()
            .map(|cwd| cwd.join("scripts").join("run-elite-sync-gate.py"));
        script_path.as_ref().map(|p| p.exists()).unwrap_or(false)
    };

    serde_json::json!({
        "provenance": {
            "path": provenance_path.display().to_string(),
            "exists": provenance_exists,
            "key_id": provenance_key_id,
        },
        "route_learning": {
            "path": route_path.display().to_string(),
            "entries": route_entries,
            "ttl_secs": route_learning_ttl_secs(),
            "half_life_secs": route_learning_half_life_secs(),
            "saved_at_unix_ms": route_state.as_ref().map(|s| s.saved_at_unix_ms),
        },
        "route_health": {
            "path": route_health_path.display().to_string(),
            "available": route_health_summary.is_some(),
            "summary": route_health_summary,
        },
        "tool_policy": {
            "mode": policy_mode,
            "preset": policy_preset,
            "counters_path": policy_counters_path.display().to_string(),
            "counters": policy_counters,
        },
        "elite_gate": {
            "command": elite_gate_script,
            "script_available": gate_available,
        }
    })
}

async fn run_doctor(
    cli: Cli,
    deep: bool,
    self_heal: bool,
    snapshot: bool,
    snapshot_path: Option<String>,
    bundle: bool,
) -> Result<(), AgentError> {
    println!("Hermes Agent Ultra — System Check");
    println!("===========================\n");

    let mut checks: Vec<serde_json::Value> = Vec::new();
    let config_dir = hermes_config::hermes_home();
    let self_heal_actions = if self_heal {
        println!("Self-heal actions:");
        let actions = run_doctor_self_heal(&cli);
        for action in &actions {
            let status = action
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let detail = action.get("detail").and_then(|v| v.as_str()).unwrap_or("");
            println!("  - {}: {}", status, detail);
        }
        println!();
        checks.push(serde_json::json!({
            "name": "self_heal",
            "ok": actions.iter().all(|a| a.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)),
            "actions": actions,
        }));
        actions
    } else {
        Vec::new()
    };

    let config_dir_ok = config_dir.exists();
    print!("Config directory ({})... ", config_dir.display());
    if config_dir_ok {
        println!("✓");
    } else {
        println!("✗ (run `hermes-ultra setup`)");
    }
    checks.push(serde_json::json!({
        "name": "config_dir",
        "ok": config_dir_ok,
        "path": config_dir.display().to_string()
    }));

    let config_path = config_dir.join("config.yaml");
    let config_yaml_ok = config_path.exists();
    print!("config.yaml... ");
    if config_yaml_ok {
        println!("✓");
    } else {
        println!("✗ (run `hermes-ultra setup`)");
    }
    checks.push(serde_json::json!({
        "name": "config_yaml",
        "ok": config_yaml_ok,
        "path": config_path.display().to_string()
    }));

    let env_path = config_dir.join(".env");
    let project_env = std::env::current_dir()
        .ok()
        .map(|p| p.join(".env"))
        .filter(|p| p.exists());
    let env_ok = env_path.exists() || project_env.is_some();
    print!("~/.hermes/.env... ");
    if env_path.exists() {
        println!("✓");
    } else if let Some(ref p) = project_env {
        println!("✓ (using fallback {})", p.display());
    } else {
        println!("✗ (run `hermes-ultra setup`)");
    }
    checks.push(serde_json::json!({
        "name": "env_file",
        "ok": env_ok,
        "path": env_path.display().to_string(),
        "fallback": project_env.as_ref().map(|p| p.display().to_string()),
    }));

    let soul_path = config_dir.join("SOUL.md");
    let soul_ok = soul_path.exists();
    print!("SOUL.md persona file... ");
    if soul_ok {
        println!("✓");
    } else {
        println!("✗ (will be created by `hermes-ultra setup` or installer)");
    }
    checks.push(serde_json::json!({
        "name": "soul_md",
        "ok": soul_ok,
        "path": soul_path.display().to_string()
    }));

    let env_file = config_dir.join(".env");
    let project_env_file = std::env::current_dir().ok().map(|p| p.join(".env"));
    let has_key = |key: &str| -> bool {
        std::env::var(key)
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
            || read_env_key(&env_file, key)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            || project_env_file
                .as_ref()
                .and_then(|p| read_env_key(p, key))
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
    };

    let api_checks = [
        ("HERMES_OPENAI_API_KEY", "OpenAI (Hermes)"),
        ("OPENAI_API_KEY", "OpenAI"),
        ("ANTHROPIC_API_KEY", "Anthropic"),
        ("OPENROUTER_API_KEY", "OpenRouter"),
        ("NOUS_API_KEY", "Nous"),
        ("EXA_API_KEY", "Exa (web search)"),
        ("FIRECRAWL_API_KEY", "Firecrawl (web extract)"),
    ];

    println!("\nAPI Keys:");
    for (env_var, name) in &api_checks {
        let ok = has_key(env_var);
        print!("  {} ({})... ", name, env_var);
        if ok {
            println!("✓");
        } else {
            println!("✗ (not set)");
        }
        checks.push(serde_json::json!({
            "name": format!("api_key_{env_var}"),
            "ok": ok
        }));
    }

    println!("\nExternal tools:");
    let tool_checks = [
        ("docker", "Docker", false),
        ("ssh", "SSH", false),
        ("git", "Git", false),
        ("node", "Node.js", true),
        ("agent-browser", "agent-browser", true),
    ];

    for (cmd, name, optional) in &tool_checks {
        print!("  {}... ", name);
        let ok = match tokio::process::Command::new("which")
            .arg(cmd)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                println!("✓");
                true
            }
            _ if *optional => {
                println!("(optional, not found)");
                true
            }
            _ => {
                println!("✗ (not found)");
                false
            }
        };
        checks.push(serde_json::json!({
            "name": format!("bin_{cmd}"),
            "ok": ok,
            "optional": optional
        }));
    }

    let mut config_summary = serde_json::json!({
        "loaded": false
    });
    let mut loaded_config: Option<GatewayConfig> = None;
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
            loaded_config = Some(config.clone());
            config_summary = serde_json::json!({
                "loaded": true,
                "model": config.model,
                "max_turns": config.max_turns,
                "enabled_platforms": platform_count,
            });
            checks.push(serde_json::json!({
                "name": "config_load",
                "ok": true
            }));
        }
        Err(e) => {
            println!("✗ ({})", e);
            checks.push(serde_json::json!({
                "name": "config_load",
                "ok": false,
                "error": e.to_string()
            }));
        }
    }

    println!("\nLocal backend endpoints:");
    for provider in [
        "ollama-local",
        "llama-cpp",
        "vllm",
        "mlx",
        "apple-ane",
        "sglang",
        "tgi",
    ] {
        let configured = loaded_config
            .as_ref()
            .and_then(|cfg| cfg.llm_providers.get(provider))
            .and_then(|entry| entry.base_url.clone())
            .filter(|value| !value.trim().is_empty());
        let env_override = local_backend_base_url_env_var(provider)
            .and_then(|name| std::env::var(name).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let base_url = configured
            .or(env_override)
            .or_else(|| setup_provider_default_base_url(provider).map(ToString::to_string));

        let (reachable, probed_url) = if let Some(url) = base_url.clone() {
            let models_url = format!("{}/models", url.trim_end_matches('/'));
            let ok = reqwest::Client::new()
                .get(models_url.as_str())
                .timeout(std::time::Duration::from_millis(900))
                .send()
                .await
                .map(|resp| resp.status().is_success())
                .unwrap_or(false);
            (ok, Some(models_url))
        } else {
            (false, None)
        };

        println!(
            "  {:<12} ... {}",
            provider,
            if reachable {
                "✓ reachable"
            } else {
                "(optional, endpoint not reachable)"
            }
        );
        checks.push(serde_json::json!({
            "name": format!("local_backend_{provider}"),
            "ok": true,
            "provider": provider,
            "base_url": base_url,
            "probe_url": probed_url,
            "reachable": reachable,
            "optional": true
        }));
    }

    if deep {
        println!("\nDeep diagnostics:");
        let svc = gateway_service_status()?;
        let svc_ok = svc.is_some();
        println!(
            "  gateway service... {}",
            if svc_ok { "✓" } else { "(not detected)" }
        );
        checks.push(serde_json::json!({
            "name": "gateway_service_status",
            "ok": true,
            "detail": svc
        }));

        let pid_path = gateway_pid_path_for_cli(&cli);
        let pid = read_gateway_pid(&pid_path);
        let pid_alive = pid.map(gateway_pid_is_alive).unwrap_or(false);
        println!(
            "  gateway pid... {}",
            if pid_alive { "✓" } else { "(not running)" }
        );
        checks.push(serde_json::json!({
            "name": "gateway_pid",
            "ok": pid_alive,
            "pid": pid,
            "pid_path": pid_path.display().to_string()
        }));

        let cl_health = reqwest::Client::new()
            .get("http://127.0.0.1:8075/health")
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map(|resp| resp.status().is_success())
            .unwrap_or(false);
        println!(
            "  contextlattice health... {}",
            if cl_health { "✓" } else { "✗" }
        );
        checks.push(serde_json::json!({
            "name": "contextlattice_health",
            "ok": cl_health,
            "url": "http://127.0.0.1:8075/health"
        }));

        let replay_dir = hermes_state_root(&cli).join("logs").join("replay");
        let replay_summaries = replay_integrity_summaries(&replay_dir, 5);
        let replay_count = std::fs::read_dir(&replay_dir)
            .map(|it| {
                it.filter_map(|e| e.ok().filter(|e| e.path().is_file()).map(|_| ()))
                    .count()
            })
            .unwrap_or(0usize);
        let replay_chain_ok = replay_summaries
            .iter()
            .all(|entry| entry.hash_chain_ok && entry.invalid_lines == 0);
        println!(
            "  replay traces... {} ({} files, chain {})",
            if replay_count > 0 { "✓" } else { "(none)" },
            replay_count,
            if replay_chain_ok { "ok" } else { "warn" }
        );
        checks.push(serde_json::json!({
            "name": "replay_traces",
            "ok": true,
            "dir": replay_dir.display().to_string(),
            "count": replay_count,
            "chain_ok": replay_chain_ok,
            "summaries": replay_summaries
        }));
    }

    let elite = build_elite_doctor_diagnostics(&cli);
    println!("\nElite diagnostics:");
    println!(
        "  provenance key... {}",
        if elite["provenance"]["exists"].as_bool().unwrap_or(false) {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "  route-learning entries... {}",
        elite["route_learning"]["entries"].as_u64().unwrap_or(0)
    );
    println!(
        "  route-health... {}",
        if elite["route_health"]["available"]
            .as_bool()
            .unwrap_or(false)
        {
            elite["route_health"]["summary"]["overall"]
                .as_str()
                .unwrap_or("available")
        } else {
            "(not generated)"
        }
    );
    println!(
        "  tool-policy mode/preset... {}/{}",
        elite["tool_policy"]["mode"].as_str().unwrap_or("unknown"),
        elite["tool_policy"]["preset"].as_str().unwrap_or("unknown")
    );
    println!(
        "  elite gate script... {}",
        if elite["elite_gate"]["script_available"]
            .as_bool()
            .unwrap_or(false)
        {
            "✓"
        } else {
            "✗"
        }
    );
    checks.push(serde_json::json!({
        "name": "elite_diagnostics",
        "ok": true,
        "details": elite,
    }));

    let snapshot_payload = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "deep": deep,
        "self_heal": self_heal,
        "self_heal_actions": self_heal_actions,
        "state_root": hermes_state_root(&cli).display().to_string(),
        "checks": checks,
        "config_summary": config_summary,
        "elite": build_elite_doctor_diagnostics(&cli),
    });

    let mut snapshot_written: Option<PathBuf> = None;
    if snapshot || bundle {
        let out = write_doctor_snapshot(&cli, &snapshot_payload, snapshot_path.as_deref())?;
        println!("\nDoctor snapshot: {}", out.display());
        if let Ok(snapshot_bytes) = std::fs::read(&out) {
            match sign_artifact_bytes(&cli, &snapshot_bytes, true)
                .and_then(|sig| write_provenance_sidecar(&out, &sig))
            {
                Ok(sig_path) => {
                    println!("Snapshot signature: {}", sig_path.display());
                    checks.push(serde_json::json!({
                        "name": "snapshot_provenance",
                        "ok": true,
                        "signature_path": sig_path.display().to_string(),
                    }));
                }
                Err(err) => {
                    checks.push(serde_json::json!({
                        "name": "snapshot_provenance",
                        "ok": false,
                        "error": err.to_string(),
                    }));
                }
            }
        }
        snapshot_written = Some(out);
    }

    if bundle {
        let snapshot_path = snapshot_written.as_ref().ok_or_else(|| {
            AgentError::Config("doctor bundle requires snapshot path".to_string())
        })?;
        let bundle_path = build_doctor_support_bundle(&cli, snapshot_path)?;
        println!("Support bundle: {}", bundle_path.display());
    }

    println!("\nDone.");
    Ok(())
}

fn run_doctor_self_heal(cli: &Cli) -> Vec<serde_json::Value> {
    let mut actions = Vec::new();
    let state_root = hermes_state_root(cli);
    let required_dirs = [
        state_root.clone(),
        state_root.join("profiles"),
        state_root.join("sessions"),
        state_root.join("logs"),
        state_root.join("skills"),
        state_root.join("auth"),
        state_root.join("snapshots"),
    ];

    for dir in required_dirs {
        if dir.exists() {
            actions.push(serde_json::json!({
                "ok": true,
                "status": "exists",
                "detail": format!("directory {}", dir.display()),
            }));
            continue;
        }
        match std::fs::create_dir_all(&dir) {
            Ok(_) => actions.push(serde_json::json!({
                "ok": true,
                "status": "created",
                "detail": format!("directory {}", dir.display()),
            })),
            Err(err) => actions.push(serde_json::json!({
                "ok": false,
                "status": "error",
                "detail": format!("directory {}: {}", dir.display(), err),
            })),
        }
    }

    let pid_path = gateway_pid_path_for_cli(cli);
    if pid_path.exists() {
        match read_gateway_pid(&pid_path) {
            Some(pid) if !gateway_pid_is_alive(pid) => match std::fs::remove_file(&pid_path) {
                Ok(_) => actions.push(serde_json::json!({
                    "ok": true,
                    "status": "fixed",
                    "detail": format!("removed stale gateway pid file {} (pid {})", pid_path.display(), pid),
                })),
                Err(err) => actions.push(serde_json::json!({
                    "ok": false,
                    "status": "error",
                    "detail": format!("remove stale pid {} failed: {}", pid_path.display(), err),
                })),
            },
            Some(pid) => actions.push(serde_json::json!({
                "ok": true,
                "status": "noop",
                "detail": format!("gateway pid {} is active", pid),
            })),
            None => actions.push(serde_json::json!({
                "ok": true,
                "status": "noop",
                "detail": format!("pid file {} is unreadable; left unchanged", pid_path.display()),
            })),
        }
    }

    let vault_path = secret_vault_path_for_cli(cli);
    if vault_path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::metadata(&vault_path) {
                Ok(meta) => {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode != 0o600 {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o600);
                        match std::fs::set_permissions(&vault_path, perms) {
                            Ok(_) => actions.push(serde_json::json!({
                                "ok": true,
                                "status": "fixed",
                                "detail": format!("normalized permissions on {} to 600", vault_path.display()),
                            })),
                            Err(err) => actions.push(serde_json::json!({
                                "ok": false,
                                "status": "error",
                                "detail": format!("set permissions on {} failed: {}", vault_path.display(), err),
                            })),
                        }
                    } else {
                        actions.push(serde_json::json!({
                            "ok": true,
                            "status": "noop",
                            "detail": format!("permissions already secure on {}", vault_path.display()),
                        }));
                    }
                }
                Err(err) => actions.push(serde_json::json!({
                    "ok": false,
                    "status": "error",
                    "detail": format!("metadata {} failed: {}", vault_path.display(), err),
                })),
            }
        }
        #[cfg(not(unix))]
        {
            actions.push(serde_json::json!({
                "ok": true,
                "status": "noop",
                "detail": format!("permission normalization skipped on non-unix for {}", vault_path.display()),
            }));
        }
    }

    actions
}

fn write_doctor_snapshot(
    cli: &Cli,
    snapshot_payload: &serde_json::Value,
    requested_path: Option<&str>,
) -> Result<PathBuf, AgentError> {
    let path = if let Some(raw) = requested_path.map(str::trim).filter(|s| !s.is_empty()) {
        PathBuf::from(raw)
    } else {
        hermes_state_root(cli).join("snapshots").join(format!(
            "doctor-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let body = serde_json::to_string_pretty(snapshot_payload)
        .map_err(|e| AgentError::Config(format!("serialize doctor snapshot: {}", e)))?;
    std::fs::write(&path, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
    Ok(path)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProvenanceSignature {
    generated_at: String,
    algorithm: String,
    key_id: String,
    artifact_sha256: String,
    signature_hex: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ProvenanceVerification {
    ok: bool,
    code: String,
    key_id: Option<String>,
    artifact_sha256: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct RouteLearningStatsRecord {
    samples: u32,
    success_rate: f64,
    avg_latency_ms: f64,
    consecutive_failures: u32,
    updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct RouteLearningStateRecord {
    schema_version: u32,
    saved_at_unix_ms: i64,
    entries: std::collections::HashMap<String, RouteLearningStatsRecord>,
}

fn provenance_key_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("auth").join("provenance.key")
}

fn parse_provenance_key_material(raw: &str) -> Result<Vec<u8>, AgentError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(AgentError::Config(
            "empty provenance key material".to_string(),
        ));
    }
    let is_hex = s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex {
        return hex::decode(s)
            .map_err(|e| AgentError::Config(format!("decode provenance hex key: {e}")));
    }
    if let Ok(bytes) = BASE64_STANDARD.decode(s.as_bytes()) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    Ok(s.as_bytes().to_vec())
}

fn load_or_create_provenance_key(cli: &Cli, allow_create: bool) -> Result<Vec<u8>, AgentError> {
    if let Ok(raw_env) = std::env::var("HERMES_PROVENANCE_SIGNING_KEY") {
        let bytes = parse_provenance_key_material(&raw_env)?;
        if bytes.len() < 16 {
            return Err(AgentError::Config(
                "HERMES_PROVENANCE_SIGNING_KEY must be at least 16 bytes".to_string(),
            ));
        }
        return Ok(bytes);
    }

    let path = provenance_key_path_for_cli(cli);
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
        let bytes = parse_provenance_key_material(&raw)?;
        if bytes.len() < 16 {
            return Err(AgentError::Config(format!(
                "provenance key in {} must be at least 16 bytes",
                path.display()
            )));
        }
        return Ok(bytes);
    }

    if !allow_create {
        return Err(AgentError::Config(format!(
            "provenance key not found at {} (set HERMES_PROVENANCE_SIGNING_KEY or run doctor snapshot/bundle once)",
            path.display()
        )));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let mut key_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut key_bytes);
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
    Ok(key_bytes.to_vec())
}

fn sign_artifact_bytes(
    cli: &Cli,
    bytes: &[u8],
    allow_create_key: bool,
) -> Result<ProvenanceSignature, AgentError> {
    use hmac::Mac as _;

    let key = load_or_create_provenance_key(cli, allow_create_key)?;
    let artifact_hash_bytes = Sha256::digest(bytes);
    let artifact_sha256 = hex::encode(artifact_hash_bytes);
    let key_id = {
        let key_hash = Sha256::digest(&key);
        let full = hex::encode(key_hash);
        full.chars().take(16).collect::<String>()
    };
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
        .map_err(|e| AgentError::Config(format!("init provenance hmac: {e}")))?;
    mac.update(artifact_sha256.as_bytes());
    let signature_hex = hex::encode(mac.finalize().into_bytes());
    Ok(ProvenanceSignature {
        generated_at: chrono::Utc::now().to_rfc3339(),
        algorithm: "hmac-sha256".to_string(),
        key_id,
        artifact_sha256,
        signature_hex,
    })
}

fn provenance_sidecar_path_for_artifact(path: &Path) -> PathBuf {
    let filename = path
        .file_name()
        .map(|f| format!("{}.sig.json", f.to_string_lossy()))
        .unwrap_or_else(|| "artifact.sig.json".to_string());
    path.parent()
        .map(|p| p.join(&filename))
        .unwrap_or_else(|| PathBuf::from(filename))
}

fn write_provenance_sidecar(path: &Path, sig: &ProvenanceSignature) -> Result<PathBuf, AgentError> {
    let sidecar = provenance_sidecar_path_for_artifact(path);
    let body = serde_json::to_string_pretty(sig)
        .map_err(|e| AgentError::Config(format!("serialize provenance sidecar: {e}")))?;
    std::fs::write(&sidecar, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", sidecar.display(), e)))?;
    Ok(sidecar)
}

fn verify_artifact_provenance(
    cli: &Cli,
    artifact_path: &Path,
    signature_path: Option<&Path>,
) -> Result<ProvenanceVerification, AgentError> {
    use hmac::Mac as _;

    let bytes = match std::fs::read(artifact_path) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "artifact_read_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("read {}: {}", artifact_path.display(), err)),
            });
        }
    };
    let sidecar_path = signature_path
        .map(PathBuf::from)
        .unwrap_or_else(|| provenance_sidecar_path_for_artifact(artifact_path));
    let sidecar_raw = match std::fs::read_to_string(&sidecar_path) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "signature_read_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("read {}: {}", sidecar_path.display(), err)),
            });
        }
    };
    let sig: ProvenanceSignature = match serde_json::from_str(&sidecar_raw) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "signature_parse_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("parse {}: {}", sidecar_path.display(), err)),
            });
        }
    };
    let key = match load_or_create_provenance_key(cli, false) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "key_unavailable".to_string(),
                key_id: Some(sig.key_id),
                artifact_sha256: Some(sig.artifact_sha256),
                reason: Some(err.to_string()),
            });
        }
    };
    let artifact_sha = hex::encode(Sha256::digest(&bytes));
    if artifact_sha != sig.artifact_sha256 {
        return Ok(ProvenanceVerification {
            ok: false,
            code: "artifact_sha256_mismatch".to_string(),
            key_id: Some(sig.key_id),
            artifact_sha256: Some(artifact_sha),
            reason: Some("artifact_sha256 mismatch".to_string()),
        });
    }
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
        .map_err(|e| AgentError::Config(format!("init provenance hmac: {e}")))?;
    mac.update(sig.artifact_sha256.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    if expected != sig.signature_hex {
        return Ok(ProvenanceVerification {
            ok: false,
            code: "signature_mismatch".to_string(),
            key_id: Some(sig.key_id),
            artifact_sha256: Some(sig.artifact_sha256),
            reason: Some("signature mismatch".to_string()),
        });
    }
    Ok(ProvenanceVerification {
        ok: true,
        code: "ok".to_string(),
        key_id: Some(sig.key_id),
        artifact_sha256: Some(sig.artifact_sha256),
        reason: None,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReplayIntegritySummary {
    file: String,
    checksum_sha256: Option<String>,
    events: usize,
    invalid_lines: usize,
    hash_chain_ok: bool,
    last_event_hash: Option<String>,
}

fn sha256_file_hex(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

fn replay_integrity_for_file(path: &Path) -> ReplayIntegritySummary {
    let mut events = 0usize;
    let mut invalid_lines = 0usize;
    let mut hash_chain_ok = true;
    let mut last_event_hash: Option<String> = None;
    let mut last_seq: Option<u64> = None;

    if let Ok(body) = std::fs::read_to_string(path) {
        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => {
                    invalid_lines = invalid_lines.saturating_add(1);
                    hash_chain_ok = false;
                    continue;
                }
            };
            events = events.saturating_add(1);
            let seq = parsed.get("seq").and_then(|v| v.as_u64());
            if let (Some(prev), Some(cur_seq)) = (last_seq, seq) {
                if cur_seq != prev.saturating_add(1) {
                    hash_chain_ok = false;
                }
            }
            if let Some(cur_seq) = seq {
                last_seq = Some(cur_seq);
            }
            let prev_hash = parsed
                .get("prev_hash")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let event_hash = parsed
                .get("event_hash")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if let (Some(expected_prev), Some(actual_prev)) =
                (last_event_hash.as_ref(), prev_hash.as_ref())
            {
                if expected_prev != actual_prev {
                    hash_chain_ok = false;
                }
            }
            if event_hash.is_none() {
                hash_chain_ok = false;
            }
            last_event_hash = event_hash.or(last_event_hash);
        }
    } else {
        invalid_lines = 1;
        hash_chain_ok = false;
    }

    ReplayIntegritySummary {
        file: path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string()),
        checksum_sha256: sha256_file_hex(path),
        events,
        invalid_lines,
        hash_chain_ok,
        last_event_hash,
    }
}

fn replay_integrity_summaries(replay_dir: &Path, limit: usize) -> Vec<ReplayIntegritySummary> {
    if !replay_dir.exists() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(replay_dir)
        .map(|rd| {
            rd.filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.reverse();
    files
        .into_iter()
        .take(limit)
        .map(|path| replay_integrity_for_file(&path))
        .collect()
}

fn replay_manifest_json(summaries: &[ReplayIntegritySummary]) -> serde_json::Value {
    let generated_at = if std::env::var("HERMES_DETERMINISTIC_ARTIFACTS")
        .ok()
        .map(|v| {
            let n = v.trim().to_ascii_lowercase();
            n == "1" || n == "true" || n == "yes" || n == "on"
        })
        .unwrap_or(true)
    {
        "1970-01-01T00:00:00Z".to_string()
    } else {
        chrono::Utc::now().to_rfc3339()
    };
    serde_json::json!({
        "generated_at": generated_at,
        "files": summaries,
        "totals": {
            "files": summaries.len(),
            "events": summaries.iter().map(|s| s.events).sum::<usize>(),
            "invalid_lines": summaries.iter().map(|s| s.invalid_lines).sum::<usize>(),
            "hash_chain_ok": summaries.iter().all(|s| s.hash_chain_ok && s.invalid_lines == 0),
        }
    })
}

fn append_bundle_bytes(
    tar: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
    name: &str,
    bytes: &[u8],
    deterministic: bool,
) -> Result<(), AgentError> {
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    if deterministic {
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
    }
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .map_err(|e| AgentError::Io(format!("append {}: {}", name, e)))
}

fn build_doctor_support_bundle_with_options(
    cli: &Cli,
    snapshot_path: &Path,
    output_path: Option<&Path>,
    deterministic: bool,
) -> Result<PathBuf, AgentError> {
    let reports_dir = debug_reports_dir_for_cli(cli);
    std::fs::create_dir_all(&reports_dir)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", reports_dir.display(), e)))?;
    let bundle_path = output_path.map(PathBuf::from).unwrap_or_else(|| {
        reports_dir.join(format!(
            "support-bundle-{}.tar.gz",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
    });
    if let Some(parent) = bundle_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let file = std::fs::File::create(&bundle_path)
        .map_err(|e| AgentError::Io(format!("create {}: {}", bundle_path.display(), e)))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);

    let snapshot_bytes = std::fs::read(snapshot_path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", snapshot_path.display(), e)))?;
    append_bundle_bytes(
        &mut tar,
        "doctor/snapshot.json",
        &snapshot_bytes,
        deterministic,
    )?;

    let report = collect_debug_report(cli, 200)?;
    append_bundle_bytes(
        &mut tar,
        "doctor/debug-report.md",
        report.as_bytes(),
        deterministic,
    )?;

    let state_root = hermes_state_root(cli);
    let log_files = [
        (
            "logs/hermes.log",
            state_root.join("logs").join("hermes.log"),
        ),
        (
            "logs/mcp-stderr.log",
            state_root.join("logs").join("mcp-stderr.log"),
        ),
    ];
    for (name, path) in log_files {
        if path.exists() {
            let bytes = std::fs::read(&path)
                .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
            append_bundle_bytes(&mut tar, &format!("doctor/{name}"), &bytes, deterministic)?;
        }
    }

    let replay_dir = state_root.join("logs").join("replay");
    let mut replay_manifest_entries: Vec<ReplayIntegritySummary> = Vec::new();
    if replay_dir.exists() {
        let mut replay_files: Vec<PathBuf> = std::fs::read_dir(&replay_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        replay_files.sort();
        replay_files.reverse();
        for path in replay_files.into_iter().take(5) {
            if path.is_file() {
                replay_manifest_entries.push(replay_integrity_for_file(&path));
                let name = format!(
                    "doctor/replay/{}",
                    path.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "replay.jsonl".to_string())
                );
                let bytes = std::fs::read(&path)
                    .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
                append_bundle_bytes(&mut tar, &name, &bytes, deterministic)?;
            }
        }
    }

    let manifest = replay_manifest_json(&replay_manifest_entries);
    let manifest_body = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| AgentError::Config(format!("serialize replay manifest: {}", e)))?;
    append_bundle_bytes(
        &mut tar,
        "doctor/replay/manifest.json",
        manifest_body.as_slice(),
        deterministic,
    )?;

    if let Ok(sig) = sign_artifact_bytes(cli, &manifest_body, true) {
        let sig_body = serde_json::to_vec_pretty(&sig)
            .map_err(|e| AgentError::Config(format!("serialize replay signature: {}", e)))?;
        append_bundle_bytes(
            &mut tar,
            "doctor/replay/manifest.sig.json",
            sig_body.as_slice(),
            deterministic,
        )?;
    }

    tar.finish()
        .map_err(|e| AgentError::Io(format!("finalize {}: {}", bundle_path.display(), e)))?;
    Ok(bundle_path)
}

fn build_doctor_support_bundle(cli: &Cli, snapshot_path: &Path) -> Result<PathBuf, AgentError> {
    build_doctor_support_bundle_with_options(cli, snapshot_path, None, false)
}

/// Handle `hermes update`.
async fn run_update(_check: bool) -> Result<(), AgentError> {
    println!("Hermes Agent v{}", env!("CARGO_PKG_VERSION"));
    println!("{}", hermes_cli::update::check_for_updates().await?);
    Ok(())
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
    let verification = verify_artifact_provenance(&cli, &artifact, signature_path.as_deref())?;
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

fn route_learning_state_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-learning.json")
}

fn route_learning_ttl_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(7 * 24 * 60 * 60)
}

fn route_learning_half_life_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(24 * 60 * 60)
}

fn route_learning_effective_stats(
    stats: &RouteLearningStatsRecord,
    now_ms: i64,
) -> Option<RouteLearningStatsRecord> {
    if stats.samples == 0 {
        return None;
    }
    let mut out = stats.clone();
    if out.updated_at_unix_ms <= 0 {
        return Some(out);
    }
    let age_ms = now_ms.saturating_sub(out.updated_at_unix_ms).max(0);
    let ttl_secs = route_learning_ttl_secs();
    if ttl_secs > 0 && age_ms >= ttl_secs.saturating_mul(1000) {
        return None;
    }
    let half_life_secs = route_learning_half_life_secs();
    if half_life_secs <= 0 || age_ms <= 0 {
        return Some(out);
    }
    let half_life_ms = (half_life_secs.saturating_mul(1000)) as f64;
    let decay = (0.5_f64)
        .powf((age_ms as f64) / half_life_ms)
        .clamp(0.0, 1.0);
    let baseline_success = 0.90;
    let baseline_latency = 1800.0;
    out.success_rate = baseline_success + (out.success_rate - baseline_success) * decay;
    out.avg_latency_ms = baseline_latency + (out.avg_latency_ms - baseline_latency) * decay;
    out.consecutive_failures = ((out.consecutive_failures as f64) * decay).round() as u32;
    out.samples = ((out.samples as f64) * decay).round().max(1.0) as u32;
    Some(out)
}

fn route_learning_score(stats: &RouteLearningStatsRecord) -> f64 {
    let success_rate = stats.success_rate;
    let latency_score = (1.0 / (1.0 + (stats.avg_latency_ms / 2500.0))).clamp(0.05, 1.0);
    let failure_penalty = (stats.consecutive_failures as f64 * 0.08).min(0.35);
    let exploration_bonus = {
        let coverage = (stats.samples.min(20) as f64) / 20.0;
        (1.0 - coverage) * 0.03
    };
    (success_rate * 0.60) + (latency_score * 0.30) + exploration_bonus - failure_penalty
}

fn load_route_learning_state_for_cli(path: &Path) -> Result<RouteLearningStateRecord, AgentError> {
    if !path.exists() {
        return Ok(RouteLearningStateRecord {
            schema_version: 1,
            saved_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            entries: std::collections::HashMap::new(),
        });
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

async fn run_route_learning(
    cli: Cli,
    action: Option<String>,
    json: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let path = route_learning_state_path_for_cli(&cli);
    match action.as_str() {
        "reset" | "clear" => {
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "path": path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-learning state cleared: {}", path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" => {}
        _ => {
            return Err(AgentError::Config(format!(
                "route-learning: unsupported action '{}'; use show/list/inspect/reset/clear",
                action
            )))
        }
    }

    let state = load_route_learning_state_for_cli(&path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut rows: Vec<(String, RouteLearningStatsRecord, f64)> = state
        .entries
        .iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(stats, now_ms).map(|effective| {
                (
                    key.clone(),
                    effective.clone(),
                    route_learning_score(&effective),
                )
            })
        })
        .collect();
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    if json {
        let body = serde_json::json!({
            "path": path.display().to_string(),
            "ttl_secs": route_learning_ttl_secs(),
            "half_life_secs": route_learning_half_life_secs(),
            "saved_at_unix_ms": state.saved_at_unix_ms,
            "entries": rows.iter().map(|(key, stats, score)| {
                serde_json::json!({
                    "key": key,
                    "score": score,
                    "stats": stats,
                })
            }).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&body)
                .map_err(|e| AgentError::Config(format!("serialize route-learning json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-learning state: {}", path.display());
    println!(
        "TTL={}s half_life={}s entries={}",
        route_learning_ttl_secs(),
        route_learning_half_life_secs(),
        rows.len()
    );
    if rows.is_empty() {
        println!("(no learned routes yet)");
        return Ok(());
    }
    println!();
    println!(
        "{:<42}  {:>7}  {:>8}  {:>10}  {:>8}  {:>14}",
        "ROUTE", "SCORE", "SUCCESS", "LAT_MS", "FAILURES", "UPDATED_AT_MS"
    );
    for (key, stats, score) in rows {
        println!(
            "{:<42}  {:>7.3}  {:>7.2}%  {:>10.1}  {:>8}  {:>14}",
            key,
            score,
            stats.success_rate * 100.0,
            stats.avg_latency_ms,
            stats.consecutive_failures,
            stats.updated_at_unix_ms
        );
    }
    Ok(())
}

fn route_health_state_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-health.json")
}

fn route_autotune_state_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-autotune.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RouteHealthEntry {
    key: String,
    health_score: f64,
    tier: String,
    reasons: Vec<String>,
    stats: RouteLearningStatsRecord,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RouteAutotunePlan {
    generated_at: String,
    learning_path: String,
    health_report_path: String,
    env_path: String,
    summary: serde_json::Value,
    confidence: String,
    reasons: Vec<String>,
    overrides: std::collections::BTreeMap<String, String>,
}

fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn clamp_i64(value: i64, min: i64, max: i64) -> i64 {
    value.max(min).min(max)
}

fn build_route_autotune_plan(
    cli: &Cli,
    learning_path: &Path,
    report_path: &Path,
    entries: &[RouteHealthEntry],
    summary: &serde_json::Value,
) -> RouteAutotunePlan {
    let total = entries.len() as f64;
    let healthy = summary.get("healthy").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let watch = summary.get("watch").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let degraded = summary
        .get("degraded")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as f64;
    let critical = summary
        .get("critical")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as f64;
    let avg_score = summary
        .get("average_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let unhealthy_ratio = if total > 0.0 {
        (degraded + critical) / total
    } else {
        0.0
    };
    let watch_ratio = if total > 0.0 { watch / total } else { 0.0 };

    let mut reasons = Vec::new();
    if total < 3.0 {
        reasons.push("low_evidence_sample".to_string());
    }
    if critical > 0.0 {
        reasons.push("critical_routes_detected".to_string());
    } else if degraded > 0.0 {
        reasons.push("degraded_routes_detected".to_string());
    } else if watch > 0.0 {
        reasons.push("watch_routes_detected".to_string());
    } else if healthy > 0.0 {
        reasons.push("routes_healthy".to_string());
    } else {
        reasons.push("no_routes_learned".to_string());
    }
    if avg_score < 0.45 {
        reasons.push("average_health_low".to_string());
    } else if avg_score >= 0.75 {
        reasons.push("average_health_high".to_string());
    }

    let confidence = if total >= 12.0 {
        "high"
    } else if total >= 5.0 {
        "medium"
    } else {
        "low"
    };

    let cheap_bias = if critical > 0.0 {
        0.16
    } else if unhealthy_ratio >= 0.50 {
        0.14
    } else if unhealthy_ratio >= 0.25 || watch_ratio > 0.45 {
        0.11
    } else if avg_score >= 0.78 {
        0.06
    } else {
        0.08
    };
    let switch_margin = if critical > 0.0 {
        0.07
    } else if degraded > 0.0 {
        0.05
    } else if watch > 0.0 {
        0.04
    } else {
        0.03
    };
    let alpha = if critical > 0.0 {
        0.35
    } else if degraded > 0.0 {
        0.28
    } else if watch > 0.0 {
        0.24
    } else {
        0.20
    };
    let ttl_secs = if critical > 0.0 {
        5 * 24 * 60 * 60
    } else if degraded > 0.0 {
        6 * 24 * 60 * 60
    } else {
        7 * 24 * 60 * 60
    };
    let half_life_secs = if critical > 0.0 {
        12 * 60 * 60
    } else if degraded > 0.0 {
        18 * 60 * 60
    } else if watch > 0.0 {
        22 * 60 * 60
    } else {
        24 * 60 * 60
    };

    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_ALPHA".to_string(),
        format!("{:.3}", clamp_f64(alpha, 0.01, 1.0)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS".to_string(),
        format!("{:.3}", clamp_f64(cheap_bias, -0.50, 0.50)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN".to_string(),
        format!("{:.3}", clamp_f64(switch_margin, 0.0, 0.50)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_TTL_SECS".to_string(),
        clamp_i64(ttl_secs, 0, 30 * 24 * 60 * 60).to_string(),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS".to_string(),
        clamp_i64(half_life_secs, 0, 30 * 24 * 60 * 60).to_string(),
    );

    RouteAutotunePlan {
        generated_at: chrono::Utc::now().to_rfc3339(),
        learning_path: learning_path.display().to_string(),
        health_report_path: report_path.display().to_string(),
        env_path: route_autotune_env_path_for_cli(cli).display().to_string(),
        summary: summary.clone(),
        confidence: confidence.to_string(),
        reasons,
        overrides,
    }
}

fn route_health_tier(stats: &RouteLearningStatsRecord, score: f64) -> (String, Vec<String>, f64) {
    let mut reasons = Vec::new();
    if stats.success_rate < 0.55 {
        reasons.push("low_success_rate".to_string());
    } else if stats.success_rate < 0.72 {
        reasons.push("recovering_success_rate".to_string());
    }
    if stats.consecutive_failures >= 5 {
        reasons.push("failure_streak_critical".to_string());
    } else if stats.consecutive_failures >= 3 {
        reasons.push("failure_streak_watch".to_string());
    }
    if stats.avg_latency_ms > 5000.0 {
        reasons.push("high_latency".to_string());
    } else if stats.avg_latency_ms > 3000.0 {
        reasons.push("latency_watch".to_string());
    }

    let health_score = ((score + 0.30) / 1.20).clamp(0.0, 1.0);
    let tier = if stats.consecutive_failures >= 5 || stats.success_rate < 0.55 {
        "critical"
    } else if health_score >= 0.72 {
        "healthy"
    } else if health_score >= 0.52 {
        "watch"
    } else if health_score >= 0.35 {
        "degraded"
    } else {
        "critical"
    };
    (tier.to_string(), reasons, health_score)
}

async fn run_route_health(cli: Cli, action: Option<String>, json: bool) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let report_path = route_health_state_path_for_cli(&cli);

    match action.as_str() {
        "reset" | "clear" => {
            if report_path.exists() {
                std::fs::remove_file(&report_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", report_path.display(), e))
                })?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "path": report_path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-health report cleared: {}", report_path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" => {}
        _ => {
            return Err(AgentError::Config(format!(
                "route-health: unsupported action '{}'; use show/list/inspect/reset/clear",
                action
            )))
        }
    }

    let learning_path = route_learning_state_path_for_cli(&cli);
    let state = load_route_learning_state_for_cli(&learning_path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut entries: Vec<RouteHealthEntry> = state
        .entries
        .into_iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(&stats, now_ms).map(|effective| {
                let score = route_learning_score(&effective);
                let (tier, reasons, health_score) = route_health_tier(&effective, score);
                RouteHealthEntry {
                    key,
                    health_score,
                    tier,
                    reasons,
                    stats: effective,
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });

    let healthy = entries.iter().filter(|e| e.tier == "healthy").count();
    let watch = entries.iter().filter(|e| e.tier == "watch").count();
    let degraded = entries.iter().filter(|e| e.tier == "degraded").count();
    let critical = entries.iter().filter(|e| e.tier == "critical").count();
    let overall = if critical > 0 {
        "critical"
    } else if degraded > 0 {
        "degraded"
    } else if watch > 0 {
        "watch"
    } else if healthy > 0 {
        "healthy"
    } else {
        "unknown"
    };
    let avg_score = if entries.is_empty() {
        0.0
    } else {
        entries.iter().map(|e| e.health_score).sum::<f64>() / (entries.len() as f64)
    };

    let payload = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "path": report_path.display().to_string(),
        "learning_path": learning_path.display().to_string(),
        "summary": {
            "entries": entries.len(),
            "overall": overall,
            "average_score": avg_score,
            "healthy": healthy,
            "watch": watch,
            "degraded": degraded,
            "critical": critical,
        },
        "entries": entries,
    });

    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| AgentError::Config(format!("serialize route-health: {}", e)))?;
    std::fs::write(&report_path, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", report_path.display(), e)))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize route-health json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-health report: {}", report_path.display());
    println!(
        "Overall={} entries={} avg_score={:.3} (healthy={} watch={} degraded={} critical={})",
        overall,
        payload["summary"]["entries"].as_u64().unwrap_or(0),
        avg_score,
        healthy,
        watch,
        degraded,
        critical
    );
    if let Some(items) = payload["entries"].as_array() {
        if items.is_empty() {
            println!("(no routes learned yet)");
            return Ok(());
        }
        println!(
            "{:<42}  {:>7}  {:<9}  {:>8}  {:>10}  {:>8}",
            "ROUTE", "HEALTH", "TIER", "SUCCESS", "LAT_MS", "FAILURES"
        );
        for item in items {
            let key = item["key"].as_str().unwrap_or("");
            let health = item["health_score"].as_f64().unwrap_or(0.0);
            let tier = item["tier"].as_str().unwrap_or("unknown");
            let stats = item["stats"].as_object();
            let success = stats
                .and_then(|s| s.get("success_rate"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let latency = stats
                .and_then(|s| s.get("avg_latency_ms"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let failures = stats
                .and_then(|s| s.get("consecutive_failures"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            println!(
                "{:<42}  {:>7.3}  {:<9}  {:>7.2}%  {:>10.1}  {:>8}",
                key,
                health,
                tier,
                success * 100.0,
                latency,
                failures
            );
        }
    }
    Ok(())
}

async fn run_route_autotune(
    cli: Cli,
    action: Option<String>,
    apply: bool,
    strict: bool,
    json: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let route_report_path = route_health_state_path_for_cli(&cli);
    let autotune_state_path = route_autotune_state_path_for_cli(&cli);
    let autotune_env_path = route_autotune_env_path_for_cli(&cli);

    match action.as_str() {
        "reset" | "clear" => {
            if autotune_state_path.exists() {
                std::fs::remove_file(&autotune_state_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", autotune_state_path.display(), e))
                })?;
            }
            if autotune_env_path.exists() {
                std::fs::remove_file(&autotune_env_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", autotune_env_path.display(), e))
                })?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "state_path": autotune_state_path.display().to_string(),
                "env_path": autotune_env_path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-autotune artifacts cleared.");
                println!("State: {}", autotune_state_path.display());
                println!("Env:   {}", autotune_env_path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" | "plan" | "apply" => {}
        _ => {
            return Err(AgentError::Config(format!(
            "route-autotune: unsupported action '{}'; use show/list/inspect/plan/apply/reset/clear",
            action
        )))
        }
    }

    let learning_path = route_learning_state_path_for_cli(&cli);
    let state = load_route_learning_state_for_cli(&learning_path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut entries: Vec<RouteHealthEntry> = state
        .entries
        .into_iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(&stats, now_ms).map(|effective| {
                let score = route_learning_score(&effective);
                let (tier, reasons, health_score) = route_health_tier(&effective, score);
                RouteHealthEntry {
                    key,
                    health_score,
                    tier,
                    reasons,
                    stats: effective,
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });

    let healthy = entries.iter().filter(|e| e.tier == "healthy").count();
    let watch = entries.iter().filter(|e| e.tier == "watch").count();
    let degraded = entries.iter().filter(|e| e.tier == "degraded").count();
    let critical = entries.iter().filter(|e| e.tier == "critical").count();
    let overall = if critical > 0 {
        "critical"
    } else if degraded > 0 {
        "degraded"
    } else if watch > 0 {
        "watch"
    } else if healthy > 0 {
        "healthy"
    } else {
        "unknown"
    };
    let avg_score = if entries.is_empty() {
        0.0
    } else {
        entries.iter().map(|e| e.health_score).sum::<f64>() / (entries.len() as f64)
    };

    let summary = serde_json::json!({
        "entries": entries.len(),
        "overall": overall,
        "average_score": avg_score,
        "healthy": healthy,
        "watch": watch,
        "degraded": degraded,
        "critical": critical,
    });
    let plan =
        build_route_autotune_plan(&cli, &learning_path, &route_report_path, &entries, &summary);
    if strict && plan.confidence == "low" {
        return Err(AgentError::Config(
            "route-autotune strict mode requires at least 5 learned routes".to_string(),
        ));
    }

    if let Some(parent) = autotune_state_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    std::fs::write(
        &autotune_state_path,
        serde_json::to_string_pretty(&plan)
            .map_err(|e| AgentError::Config(format!("serialize route-autotune plan: {}", e)))?,
    )
    .map_err(|e| AgentError::Io(format!("write {}: {}", autotune_state_path.display(), e)))?;

    let should_apply = apply || action == "apply";
    if should_apply {
        if let Some(parent) = autotune_env_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
        }
        let mut body = String::new();
        body.push_str("# Hermes Agent Ultra route-autotune overrides\n");
        body.push_str(&format!("# generated_at={}\n", plan.generated_at));
        for (key, value) in &plan.overrides {
            body.push_str(&format!("{key}={value}\n"));
        }
        std::fs::write(&autotune_env_path, body)
            .map_err(|e| AgentError::Io(format!("write {}: {}", autotune_env_path.display(), e)))?;
    }

    let payload = serde_json::json!({
        "ok": true,
        "action": action,
        "applied": should_apply,
        "strict": strict,
        "state_path": autotune_state_path.display().to_string(),
        "env_path": autotune_env_path.display().to_string(),
        "plan": plan,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize route-autotune json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-autotune plan: {}", autotune_state_path.display());
    println!(
        "Overall={} entries={} avg_score={:.3} confidence={} applied={}",
        payload["plan"]["summary"]["overall"]
            .as_str()
            .unwrap_or("unknown"),
        payload["plan"]["summary"]["entries"].as_u64().unwrap_or(0),
        payload["plan"]["summary"]["average_score"]
            .as_f64()
            .unwrap_or(0.0),
        payload["plan"]["confidence"].as_str().unwrap_or("low"),
        if should_apply { "yes" } else { "no" },
    );
    if let Some(reasons) = payload["plan"]["reasons"].as_array() {
        if !reasons.is_empty() {
            println!(
                "Reasons: {}",
                reasons
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    println!("\nSuggested overrides:");
    if let Some(obj) = payload["plan"]["overrides"].as_object() {
        for (key, value) in obj {
            println!("  {:<44} {}", key, value.as_str().unwrap_or(""));
        }
    }
    if should_apply {
        println!(
            "\nApplied overrides file: {} (loaded automatically on next start unless env explicitly overrides a key)",
            autotune_env_path.display()
        );
    } else {
        println!("\nRun `hermes route-autotune apply --apply` to persist these overrides.");
    }
    Ok(())
}

async fn run_incident_pack(
    cli: Cli,
    snapshot: Option<String>,
    output: Option<String>,
    json: bool,
) -> Result<(), AgentError> {
    let snapshot_path = if let Some(path) = snapshot
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        if !path.exists() {
            return Err(AgentError::Config(format!(
                "incident-pack snapshot not found: {}",
                path.display()
            )));
        }
        path
    } else {
        let payload = serde_json::json!({
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "mode": "incident_pack_snapshot",
            "state_root": hermes_state_root(&cli).display().to_string(),
            "elite": build_elite_doctor_diagnostics(&cli),
        });
        let out = write_doctor_snapshot(&cli, &payload, None)?;
        if let Ok(snapshot_bytes) = std::fs::read(&out) {
            if let Ok(sig) = sign_artifact_bytes(&cli, &snapshot_bytes, true) {
                let _ = write_provenance_sidecar(&out, &sig);
            }
        }
        out
    };

    let output_path = output
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let bundle = build_doctor_support_bundle_with_options(
        &cli,
        &snapshot_path,
        output_path.as_deref(),
        true,
    )?;

    let bundle_sig_path = if let Ok(bundle_bytes) = std::fs::read(&bundle) {
        sign_artifact_bytes(&cli, &bundle_bytes, true)
            .ok()
            .and_then(|sig| write_provenance_sidecar(&bundle, &sig).ok())
            .map(|p| p.display().to_string())
    } else {
        None
    };

    let payload = serde_json::json!({
        "ok": true,
        "deterministic": true,
        "snapshot": snapshot_path.display().to_string(),
        "bundle": bundle.display().to_string(),
        "bundle_signature": bundle_sig_path,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize incident-pack json: {}", e)))?
        );
    } else {
        println!("Incident pack created: {}", bundle.display());
        println!("Snapshot: {}", snapshot_path.display());
        if let Some(sig) = payload["bundle_signature"].as_str() {
            println!("Bundle signature: {}", sig);
        }
    }
    Ok(())
}

async fn run_rotate_provenance_key(cli: Cli, json: bool) -> Result<(), AgentError> {
    let path = provenance_key_path_for_cli(&cli);
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
    OsRng.fill_bytes(&mut key_bytes);
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

    let route_health_path = route_health_state_path_for_cli(&cli);
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

fn debug_reports_dir_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("debug-reports")
}

fn prune_old_debug_reports(path: &Path, expire_days: u32) -> Result<usize, AgentError> {
    if !path.exists() {
        return Ok(0);
    }
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(expire_days as u64 * 86_400))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let mut removed = 0usize;
    for entry in std::fs::read_dir(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?
    {
        let entry = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let modified = std::fs::metadata(&p)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if modified < cutoff {
            if std::fs::remove_file(&p).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

const DEBUG_LOG_SNAPSHOT_MAX_BYTES: usize = 512 * 1024;
const DEBUG_PENDING_PASTES_FILE: &str = "pending-pastes.json";

#[derive(Debug, Clone)]
struct DebugLogSnapshot {
    tail_text: String,
    #[cfg_attr(not(test), allow(dead_code))]
    full_text: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct PendingPasteDelete {
    url: String,
    expires_at_unix: i64,
}

fn debug_pending_pastes_path(reports_dir: &Path) -> PathBuf {
    reports_dir.join(DEBUG_PENDING_PASTES_FILE)
}

fn best_effort_sweep_expired_pending_pastes(reports_dir: &Path, now_unix: i64) -> usize {
    sweep_expired_pending_pastes(reports_dir, now_unix).unwrap_or(0)
}

fn sweep_expired_pending_pastes(reports_dir: &Path, now_unix: i64) -> Result<usize, AgentError> {
    let store = debug_pending_pastes_path(reports_dir);
    if !store.exists() {
        return Ok(0);
    }
    let content = std::fs::read_to_string(&store)
        .map_err(|e| AgentError::Io(format!("read {}: {}", store.display(), e)))?;
    let entries: Vec<PendingPasteDelete> = serde_json::from_str(&content).unwrap_or_default();
    if entries.is_empty() {
        let _ = std::fs::remove_file(&store);
        return Ok(0);
    }

    let mut kept: Vec<PendingPasteDelete> = Vec::new();
    let mut removed = 0usize;
    for entry in entries {
        if entry.expires_at_unix <= now_unix {
            removed += 1;
        } else {
            kept.push(entry);
        }
    }

    if removed == 0 {
        return Ok(0);
    }

    if kept.is_empty() {
        std::fs::remove_file(&store)
            .map_err(|e| AgentError::Io(format!("remove {}: {}", store.display(), e)))?;
    } else {
        let body = serde_json::to_string_pretty(&kept)
            .map_err(|e| AgentError::Config(format!("serialize pending paste store: {}", e)))?;
        std::fs::write(&store, body)
            .map_err(|e| AgentError::Io(format!("write {}: {}", store.display(), e)))?;
    }
    Ok(removed)
}

fn record_pending_paste(
    reports_dir: &Path,
    url: &str,
    expire_days: u32,
    now_unix: i64,
) -> Result<(), AgentError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let store = debug_pending_pastes_path(reports_dir);
    let mut entries: Vec<PendingPasteDelete> = if store.exists() {
        std::fs::read_to_string(&store)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<PendingPasteDelete>>(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let expires_at_unix = now_unix.saturating_add((expire_days as i64).saturating_mul(86_400));
    entries.push(PendingPasteDelete {
        url: trimmed.to_string(),
        expires_at_unix,
    });
    let body = serde_json::to_string_pretty(&entries)
        .map_err(|e| AgentError::Config(format!("serialize pending paste store: {}", e)))?;
    std::fs::write(&store, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", store.display(), e)))?;
    Ok(())
}

fn capture_debug_log_snapshot(
    log_file: &Path,
    tail_lines: usize,
    max_bytes: usize,
) -> DebugLogSnapshot {
    if !log_file.exists() {
        return DebugLogSnapshot {
            tail_text: "(file not found)".to_string(),
            full_text: None,
        };
    }

    let mut raw: Vec<u8> = Vec::new();
    let mut truncated = false;
    let read_result: Result<(), String> = (|| {
        let mut file = std::fs::File::open(log_file)
            .map_err(|e| format!("open {}: {}", log_file.display(), e))?;
        let size = file
            .metadata()
            .map_err(|e| format!("stat {}: {}", log_file.display(), e))?
            .len() as usize;
        if size == 0 {
            return Ok(());
        }

        if size <= max_bytes {
            file.read_to_end(&mut raw)
                .map_err(|e| format!("read {}: {}", log_file.display(), e))?;
            return Ok(());
        }

        let mut pos = size as u64;
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let mut total = 0usize;
        let mut newline_count = 0usize;
        let mut chunk_size = max_bytes.min(8192).max(1);
        let hard_cap = max_bytes.saturating_mul(2).max(max_bytes);

        while pos > 0
            && (total < max_bytes || newline_count < tail_lines.saturating_add(1))
            && total < hard_cap
        {
            let read_size = chunk_size.min(pos as usize);
            pos -= read_size as u64;
            file.seek(SeekFrom::Start(pos))
                .map_err(|e| format!("seek {}: {}", log_file.display(), e))?;
            let mut buf = vec![0u8; read_size];
            file.read_exact(&mut buf)
                .map_err(|e| format!("read {}: {}", log_file.display(), e))?;
            newline_count += buf.iter().filter(|b| **b == b'\n').count();
            total += buf.len();
            chunks.push(buf);
            chunk_size = (chunk_size * 2).min(65_536);
        }

        chunks.reverse();
        raw = chunks.concat();
        truncated = pos > 0;
        Ok(())
    })();

    if let Err(err) = read_result {
        return DebugLogSnapshot {
            tail_text: format!("(error reading: {err})"),
            full_text: None,
        };
    }

    let mut full_raw = raw.clone();
    if truncated && full_raw.len() > max_bytes {
        let cut = full_raw.len() - max_bytes;
        let on_boundary = cut > 0 && full_raw[cut - 1] == b'\n';
        full_raw = full_raw[cut..].to_vec();
        if !on_boundary {
            if let Some(idx) = full_raw.iter().position(|b| *b == b'\n') {
                full_raw = full_raw[idx + 1..].to_vec();
            }
        }
    }

    let text = String::from_utf8_lossy(&raw);
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return DebugLogSnapshot {
            tail_text: "(file empty)".to_string(),
            full_text: None,
        };
    }

    let start = lines.len().saturating_sub(tail_lines);
    let tail = lines.drain(start..).collect::<Vec<_>>().join("\n");
    let mut full_text = String::from_utf8_lossy(&full_raw).to_string();
    if truncated {
        full_text = format!(
            "[... truncated — showing last ~{}KB ...]\n{}",
            max_bytes / 1024,
            full_text
        );
    }
    DebugLogSnapshot {
        tail_text: tail,
        full_text: Some(full_text),
    }
}

fn collect_debug_report(cli: &Cli, lines: u32) -> Result<String, AgentError> {
    let now = chrono::Utc::now().to_rfc3339();
    let root = hermes_state_root(cli);
    let cfg_path = root.join("config.yaml");
    let log_file = root.join("logs").join("hermes.log");
    let mut report = String::new();
    report.push_str("# Hermes Debug Report\n\n");
    report.push_str(&format!("- generated_at: {}\n", now));
    report.push_str(&format!("- version: {}\n", env!("CARGO_PKG_VERSION")));
    report.push_str(&format!("- os: {}\n", std::env::consts::OS));
    report.push_str(&format!("- arch: {}\n", std::env::consts::ARCH));
    report.push_str(&format!("- state_root: {}\n", root.display()));
    report.push_str(&format!("- config_path: {}\n", cfg_path.display()));
    report.push_str(&format!("- log_path: {}\n", log_file.display()));
    if let Some(svc) = gateway_service_status()? {
        report.push_str(&format!(
            "- gateway_service: {}\n",
            svc.replace('\n', " | ")
        ));
    }
    let pid_path = gateway_pid_path_for_cli(cli);
    if let Some(pid) = read_gateway_pid(&pid_path) {
        report.push_str(&format!(
            "- gateway_pid: {} (alive={})\n",
            pid,
            gateway_pid_is_alive(pid)
        ));
    } else {
        report.push_str("- gateway_pid: none\n");
    }
    if let Ok(cfg) = load_config(cli.config_dir.as_deref()) {
        report.push_str("\n## Config Summary\n");
        report.push_str(&format!(
            "- model: {}\n",
            cfg.model.as_deref().unwrap_or("gpt-4o")
        ));
        report.push_str(&format!(
            "- personality: {}\n",
            cfg.personality.as_deref().unwrap_or("default")
        ));
        let mut enabled_platforms: Vec<String> = cfg
            .platforms
            .iter()
            .filter_map(|(k, v)| v.enabled.then_some(k.clone()))
            .collect();
        enabled_platforms.sort();
        report.push_str(&format!(
            "- enabled_platforms: {}\n",
            if enabled_platforms.is_empty() {
                "(none)".to_string()
            } else {
                enabled_platforms.join(", ")
            }
        ));
    }
    report.push_str("\n## Recent Logs\n\n```\n");
    let snapshot =
        capture_debug_log_snapshot(&log_file, lines as usize, DEBUG_LOG_SNAPSHOT_MAX_BYTES);
    report.push_str(&snapshot.tail_text);
    report.push('\n');
    report.push_str("```\n");
    Ok(report)
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
                println!("No profiles directory found. Run `hermes-ultra setup` first.");
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
    use hermes_config::session::SessionConfig;
    use hermes_config::PlatformConfig;
    use hermes_gateway::dm::DmManager;
    use hermes_gateway::{Gateway, SessionManager};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
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
    fn acp_action_from_flags_maps_entry_flags() {
        assert_eq!(
            acp_action_from_flags(None, true, false, false, false).as_deref(),
            Some("check")
        );
        assert_eq!(
            acp_action_from_flags(None, false, true, false, false).as_deref(),
            Some("setup")
        );
        assert_eq!(
            acp_action_from_flags(None, false, false, true, false).as_deref(),
            Some("setup-browser")
        );
        assert_eq!(
            acp_action_from_flags(None, false, false, false, true).as_deref(),
            Some("version")
        );
        assert_eq!(
            acp_action_from_flags(Some("restart".to_string()), false, false, false, false)
                .as_deref(),
            Some("restart")
        );
    }

    #[test]
    fn acp_setup_browser_answer_accepts_only_explicit_yes() {
        assert!(acp_setup_browser_answer_is_yes("y"));
        assert!(acp_setup_browser_answer_is_yes("YES\n"));
        assert!(!acp_setup_browser_answer_is_yes(""));
        assert!(!acp_setup_browser_answer_is_yes("no"));
    }

    #[tokio::test]
    async fn run_portal_rejects_unknown_action() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let err = run_portal(cli, Some("bogus".to_string()))
            .await
            .expect_err("unknown portal actions must fail before auth side effects");
        assert!(err.to_string().contains("Unknown portal action 'bogus'"));
        assert!(err.to_string().contains("hermes-ultra portal info"));
    }

    #[test]
    fn portal_default_runs_setup_alias() {
        for action in [
            None,
            Some(""),
            Some("  "),
            Some("setup"),
            Some("login"),
            Some("auth"),
        ] {
            assert_eq!(
                portal_action_kind(action).expect("setup portal action"),
                PortalActionKind::Setup
            );
        }
    }

    #[test]
    fn portal_info_and_status_are_status_aliases() {
        for action in [Some("info"), Some("status"), Some("check")] {
            assert_eq!(
                portal_action_kind(action).expect("info portal action"),
                PortalActionKind::Info
            );
        }
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
    fn telegram_gateway_message_preserves_group_topic_in_chat_id() {
        let incoming = TelegramIncomingMessage {
            chat_id: -1001,
            user_id: Some(42),
            username: Some("alice".to_string()),
            text: Some("topic hello".to_string()),
            message_id: 77,
            is_voice: false,
            is_photo: false,
            is_sticker: false,
            is_document: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            reply_to_message_id: None,
            message_thread_id: Some(17585),
            chat_type: hermes_gateway::platforms::telegram::ChatKind::Supergroup,
            is_group: true,
            callback_query_id: None,
            callback_data: None,
        };

        let routed = telegram_gateway_message(incoming);
        assert_eq!(routed.chat_id, "-1001:17585");
        assert_eq!(routed.user_id, "42");
        assert!(!routed.is_dm);
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
        assert_eq!(normalize_auth_provider("google-ai-studio"), "gemini");
        assert_eq!(normalize_auth_provider("step-plan"), "stepfun");
        assert_eq!(normalize_auth_provider("aigateway"), "ai-gateway");
        assert_eq!(normalize_auth_provider("moonshot"), "kimi-coding");
        assert_eq!(normalize_auth_provider("z-ai"), "zai");
        assert_eq!(normalize_auth_provider("grok"), "xai");
        assert_eq!(normalize_auth_provider("hf"), "huggingface");
        assert_eq!(normalize_auth_provider("github-models"), "copilot");
        assert_eq!(normalize_auth_provider("copilot-acp-agent"), "copilot-acp");
        assert_eq!(normalize_auth_provider("gmicloud"), "gmi");
        assert_eq!(normalize_auth_provider("arcee-ai"), "arcee");
        assert_eq!(normalize_auth_provider("mimo"), "xiaomi");
        assert_eq!(normalize_auth_provider("tencent-cloud"), "tencent-tokenhub");
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
        std::env::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID");
        std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID");
        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");

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
        assert_eq!(provider_env_var("kimi-coding"), Some("KIMI_CODING_API_KEY"));
        assert_eq!(provider_env_var("kimi"), Some("KIMI_API_KEY"));
        assert_eq!(provider_env_var("copilot"), Some("COPILOT_GITHUB_TOKEN"));
        assert_eq!(
            secret_provider_aliases("copilot"),
            vec!["copilot", "github-copilot", "github-models"]
        );
        assert_eq!(provider_env_var("gmi-cloud"), Some("GMI_API_KEY"));
        assert_eq!(
            secret_provider_aliases("gmi"),
            vec!["gmi", "gmi-cloud", "gmicloud"]
        );
        assert_eq!(provider_env_var("arcee-ai"), Some("ARCEEAI_API_KEY"));
        assert_eq!(
            secret_provider_aliases("arcee"),
            vec!["arcee", "arcee-ai", "arceeai"]
        );
        assert_eq!(provider_env_var("mimo"), Some("XIAOMI_API_KEY"));
        assert_eq!(
            secret_provider_aliases("xiaomi"),
            vec!["xiaomi", "mimo", "xiaomi-mimo"]
        );
        assert_eq!(provider_env_var("tokenhub"), Some("TOKENHUB_API_KEY"));
        assert_eq!(
            secret_provider_aliases("tencent"),
            vec![
                "tencent-tokenhub",
                "tencent",
                "tokenhub",
                "tencent-cloud",
                "tencentmaas"
            ]
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
        assert_eq!(provider_env_var("bedrock"), None);
        assert_eq!(
            secret_provider_aliases("aws-bedrock"),
            vec!["bedrock", "aws", "aws-bedrock", "amazon-bedrock", "amazon"]
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
        std::env::set_var("MATRIX_HOME_ROOM", "!env:matrix.org");
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
            Some(value) => std::env::set_var("MATRIX_HOME_ROOM", value),
            None => std::env::remove_var("MATRIX_HOME_ROOM"),
        }
    }

    #[test]
    fn build_telegram_config_reads_reply_secret_and_reactions() {
        let _guard = env_lock();
        let previous_secret = std::env::var("TELEGRAM_WEBHOOK_SECRET").ok();
        std::env::set_var("TELEGRAM_WEBHOOK_SECRET", "env-secret");

        let mut platform = PlatformConfig {
            webhook_url: Some("https://hooks.example.com/tg".to_string()),
            ..PlatformConfig::default()
        };
        platform
            .extra
            .insert("reply_to_mode".to_string(), serde_json::json!("all"));
        platform
            .extra
            .insert("reactions".to_string(), serde_json::json!(true));
        platform.extra.insert(
            "fallback_ips".to_string(),
            serde_json::json!("149.154.167.220,::1"),
        );
        platform.require_mention = Some(true);
        platform
            .extra
            .insert("guest_mode".to_string(), serde_json::json!(true));
        platform.extra.insert(
            "free_response_chats".to_string(),
            serde_json::json!(["-100", "-101"]),
        );
        platform
            .extra
            .insert("allowed_chats".to_string(), serde_json::json!("-200, -201"));
        platform.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["-300", "-301"]),
        );
        platform
            .extra
            .insert("ignored_threads".to_string(), serde_json::json!([31, "32"]));
        platform
            .extra
            .insert("allowed_topics".to_string(), serde_json::json!([8, "0"]));
        platform.extra.insert(
            "mention_patterns".to_string(),
            serde_json::json!(["^\\s*chompy\\b", "@hermes"]),
        );
        platform.extra.insert(
            "exclusive_bot_mentions".to_string(),
            serde_json::json!(true),
        );
        platform.extra.insert(
            "observe_unmentioned_group_messages".to_string(),
            serde_json::json!(true),
        );
        platform
            .extra
            .insert("text_batch_delay_ms".to_string(), serde_json::json!(125));

        let cfg = build_telegram_config(&platform, "token".to_string());
        assert_eq!(
            cfg.webhook_url.as_deref(),
            Some("https://hooks.example.com/tg")
        );
        assert_eq!(cfg.webhook_secret.as_deref(), Some("env-secret"));
        assert_eq!(cfg.reply_to_mode, "all");
        assert!(cfg.reactions);
        assert_eq!(cfg.fallback_ips, vec!["149.154.167.220", "::1"]);
        assert!(cfg.require_mention);
        assert!(cfg.guest_mode);
        assert_eq!(cfg.free_response_chats, vec!["-100", "-101"]);
        assert_eq!(cfg.allowed_chats, vec!["-200", "-201"]);
        assert_eq!(cfg.group_allowed_chats, vec!["-300", "-301"]);
        assert_eq!(cfg.ignored_threads, vec!["31", "32"]);
        assert_eq!(cfg.allowed_topics, vec!["8", "0"]);
        assert_eq!(cfg.mention_patterns, vec![r"^\s*chompy\b", "@hermes"]);
        assert!(cfg.exclusive_bot_mentions);
        assert!(cfg.observe_unmentioned_group_messages);
        assert_eq!(cfg.text_batch_delay_ms, 125);

        match previous_secret {
            Some(value) => std::env::set_var("TELEGRAM_WEBHOOK_SECRET", value),
            None => std::env::remove_var("TELEGRAM_WEBHOOK_SECRET"),
        }
    }

    #[test]
    fn build_telegram_config_maps_yaml_boolean_off_reply_mode() {
        let mut platform = PlatformConfig::default();
        platform
            .extra
            .insert("reply_to_mode".to_string(), serde_json::json!(false));

        let cfg = build_telegram_config(&platform, "token".to_string());
        assert_eq!(cfg.reply_to_mode, "off");
    }

    #[test]
    fn gateway_platform_access_policy_reads_discord_channel_lists() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut discord = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        discord
            .extra
            .insert("allowed_channels".to_string(), serde_json::json!("111, *"));
        discord.extra.insert(
            "ignored_channels".to_string(),
            serde_json::json!(["222", 333]),
        );
        config.platforms.insert("discord".to_string(), discord);

        let policies = build_gateway_platform_access_policies(&config);
        let policy = policies.get("discord").expect("discord policy");
        assert!(policy.allowed_channels.contains("111"));
        assert!(policy.allowed_channels.contains("*"));
        assert!(policy.ignored_channels.contains("222"));
        assert!(policy.ignored_channels.contains("333"));
    }

    #[test]
    fn gateway_platform_access_policy_reads_telegram_chat_lists() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        telegram
            .extra
            .insert("allowed_chats".to_string(), serde_json::json!("-100, *"));
        telegram.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["-200", -300]),
        );
        telegram
            .extra
            .insert("ignored_threads".to_string(), serde_json::json!(["31", 32]));
        config.platforms.insert("telegram".to_string(), telegram);

        let policies = build_gateway_platform_access_policies(&config);
        let policy = policies.get("telegram").expect("telegram policy");
        assert!(policy.allowed_channels.contains("-100"));
        assert!(policy.allowed_channels.contains("*"));
        assert!(policy.authorized_group_chats.contains("-200"));
        assert!(policy.authorized_group_chats.contains("-300"));
        assert!(policy.ignored_channels.contains("31"));
        assert!(policy.ignored_channels.contains("32"));
    }

    #[test]
    fn gateway_platform_access_policy_reads_dingtalk_and_matrix_aliases() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut dingtalk = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        dingtalk.extra.insert(
            "allowed_chats".to_string(),
            serde_json::json!("cidABC,cidDEF"),
        );
        config.platforms.insert("dingtalk".to_string(), dingtalk);

        let mut matrix = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        matrix.extra.insert(
            "allowed_rooms".to_string(),
            serde_json::json!(["!room1:srv", "!room2:srv"]),
        );
        config.platforms.insert("matrix".to_string(), matrix);

        let policies = build_gateway_platform_access_policies(&config);
        let dingtalk = policies.get("dingtalk").expect("dingtalk policy");
        assert!(dingtalk.allowed_channels.contains("cidABC"));
        assert!(dingtalk.allowed_channels.contains("cidDEF"));
        let matrix = policies.get("matrix").expect("matrix policy");
        assert!(matrix.allowed_channels.contains("!room1:srv"));
        assert!(matrix.allowed_channels.contains("!room2:srv"));
    }

    #[tokio::test]
    async fn gateway_dm_manager_scopes_configured_allowlists_by_platform() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        telegram.allowed_users = vec!["123".to_string()];
        config.platforms.insert("telegram".to_string(), telegram);

        let dm = build_gateway_dm_manager(&config);
        assert_eq!(
            dm.handle_dm("123", "telegram").await,
            hermes_gateway::DmDecision::Allow
        );
        assert!(matches!(
            dm.handle_dm("123", "discord").await,
            hermes_gateway::DmDecision::Pair { .. }
        ));
        assert_eq!(
            dm.handle_dm("999", "telegram").await,
            hermes_gateway::DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn gateway_dm_manager_allows_explicit_pair_with_allowlist() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut signal = PlatformConfig {
            enabled: true,
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Pair,
            ..PlatformConfig::default()
        };
        signal.allowed_users = vec!["+15550000001".to_string()];
        config.platforms.insert("signal".to_string(), signal);

        let dm = build_gateway_dm_manager(&config);
        assert!(matches!(
            dm.handle_dm("+15559999999", "signal").await,
            hermes_gateway::DmDecision::Pair { .. }
        ));
    }

    #[tokio::test]
    async fn gateway_dm_manager_global_allowlist_ignores_unauthorized_dm() {
        let mut config = hermes_config::GatewayConfig::default();
        let signal = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        config.platforms.insert("signal".to_string(), signal);
        let env = std::collections::HashMap::from([(
            "GATEWAY_ALLOWED_USERS".to_string(),
            "111111111".to_string(),
        )]);

        let dm = build_gateway_dm_manager_with_lookup(&config, |key| env.get(key).cloned());
        assert_eq!(
            dm.handle_dm("111111111", "signal").await,
            hermes_gateway::DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm("+15559999999", "signal").await,
            hermes_gateway::DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn gateway_dm_manager_dm_policy_pairing_overrides_global_allowlist_ignore() {
        let mut config = hermes_config::GatewayConfig::default();
        config.platforms.insert(
            "wecom".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        let env = std::collections::HashMap::from([
            (
                "GATEWAY_ALLOWED_USERS".to_string(),
                "admin-user".to_string(),
            ),
            ("WECOM_DM_POLICY".to_string(), "pairing".to_string()),
        ]);

        let dm = build_gateway_dm_manager_with_lookup(&config, |key| env.get(key).cloned());
        assert_eq!(
            dm.handle_dm("admin-user", "wecom").await,
            hermes_gateway::DmDecision::Allow
        );
        assert!(matches!(
            dm.handle_dm("stranger", "wecom").await,
            hermes_gateway::DmDecision::Pair { .. }
        ));
    }

    #[tokio::test]
    async fn gateway_dm_manager_dm_policy_allowlist_denies_unlisted_sender_without_pairing() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut weixin = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        weixin.allowed_users = vec!["known-user".to_string()];
        weixin
            .extra
            .insert("dm_policy".to_string(), serde_json::json!("allowlist"));
        config.platforms.insert("weixin".to_string(), weixin);

        let dm = build_gateway_dm_manager_with_lookup(&config, |_key| None);
        assert_eq!(
            dm.handle_dm("known-user", "weixin").await,
            hermes_gateway::DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm("stranger", "weixin").await,
            hermes_gateway::DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn gateway_dm_manager_group_authorization_matches_upstream_contract() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        telegram.extra.insert(
            "group_allow_from".to_string(),
            serde_json::json!(["999", "-1001878443972"]),
        );
        telegram.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["-200"]),
        );
        config.platforms.insert("telegram".to_string(), telegram);
        let mut qq = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        qq.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["group-openid-1"]),
        );
        config.platforms.insert("qq".to_string(), qq);

        let dm = build_gateway_dm_manager_with_lookup(&config, |_key| None);
        assert!(dm.is_authorized_source("telegram", "999", "-1009999999999", false));
        assert!(dm.is_authorized_source("telegram", "123", "-1001878443972", false));
        assert!(dm.is_authorized_source("telegram", "123", "-200", false));
        assert!(!dm.is_authorized_source("telegram", "999", "999", true));
        assert_eq!(
            dm.handle_dm("999", "telegram").await,
            hermes_gateway::DmDecision::Deny
        );
        assert!(dm.is_authorized_source("qqbot", "member-openid-999", "group-openid-1", false));
        assert!(!dm.is_authorized_source("qqbot", "member-openid-999", "group-openid-2", false));
    }

    #[tokio::test]
    async fn gateway_dm_manager_whatsapp_lid_mapping_authorizes_phone_allowlist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_dir = tmp.path().join("whatsapp").join("session");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        std::fs::write(
            session_dir.join("lid-mapping-15550000001.json"),
            "\"900000000000001\"",
        )
        .expect("forward mapping");
        std::fs::write(
            session_dir.join("lid-mapping-900000000000001_reverse.json"),
            "\"15550000001\"",
        )
        .expect("reverse mapping");

        let mut config = hermes_config::GatewayConfig {
            home_dir: Some(tmp.path().to_string_lossy().to_string()),
            ..hermes_config::GatewayConfig::default()
        };
        config.platforms.insert(
            "whatsapp".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        let env = std::collections::HashMap::from([(
            "WHATSAPP_ALLOWED_USERS".to_string(),
            "15550000001".to_string(),
        )]);

        let dm = build_gateway_dm_manager_with_lookup(&config, |key| env.get(key).cloned());
        assert_eq!(
            dm.handle_dm("900000000000001@lid", "whatsapp").await,
            hermes_gateway::DmDecision::Allow
        );
    }

    #[test]
    fn gateway_platform_access_policy_group_authorization_matches_env_contract() {
        let mut config = hermes_config::GatewayConfig::default();
        config.platforms.insert(
            "telegram".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        config.platforms.insert(
            "qqbot".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        let env = std::collections::HashMap::from([
            (
                "TELEGRAM_GROUP_ALLOWED_USERS".to_string(),
                "999,-1001878443972".to_string(),
            ),
            (
                "TELEGRAM_GROUP_ALLOWED_CHATS".to_string(),
                "-200".to_string(),
            ),
            (
                "QQ_GROUP_ALLOWED_USERS".to_string(),
                "group-openid-1".to_string(),
            ),
        ]);

        let policies = build_gateway_platform_access_policies_with_lookup(&config, |key| {
            env.get(key).cloned()
        });
        let telegram = policies.get("telegram").expect("telegram policy");
        assert_eq!(telegram.group_mode, GroupAccessMode::Allowlist);
        assert!(telegram.allowed_users.contains("999"));
        assert!(telegram.authorized_group_chats.contains("-1001878443972"));
        assert!(telegram.authorized_group_chats.contains("-200"));
        let qq = policies.get("qqbot").expect("qqbot policy");
        assert_eq!(qq.group_mode, GroupAccessMode::Allowlist);
        assert!(qq.authorized_group_chats.contains("group-openid-1"));
    }

    #[test]
    fn gateway_allowlist_startup_warning_matches_env_contract() {
        let env_lookup = |pairs: &[(&str, &str)]| {
            let env = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<std::collections::HashMap<_, _>>();
            gateway_allowlist_startup_would_warn_from_lookup(|key| env.get(key).cloned())
        };

        assert!(env_lookup(&[]));
        assert!(!env_lookup(&[("SIGNAL_GROUP_ALLOWED_USERS", "user1")]));
        assert!(!env_lookup(&[("TELEGRAM_ALLOW_ALL_USERS", "true")]));
        assert!(!env_lookup(&[("GATEWAY_ALLOW_ALL_USERS", "yes")]));
        assert!(env_lookup(&[("GATEWAY_ALLOW_ALL_USERS", "no")]));

        let empty_env = |_key: &str| -> Option<String> { None };
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        assert!(gateway_allowlist_startup_would_warn_with_lookup(
            &config, empty_env
        ));

        telegram.allowed_users = vec!["123".to_string()];
        config.platforms.insert("telegram".to_string(), telegram);
        assert!(!gateway_allowlist_startup_would_warn_with_lookup(
            &config, empty_env
        ));

        let mut group_config = hermes_config::GatewayConfig::default();
        let mut signal = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        signal.extra.insert(
            "group_allow_from".to_string(),
            serde_json::json!(["+15550000001"]),
        );
        group_config.platforms.insert("signal".to_string(), signal);
        assert!(!gateway_allowlist_startup_would_warn_with_lookup(
            &group_config,
            empty_env
        ));

        let mut allow_all_config = hermes_config::GatewayConfig::default();
        let mut discord = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        discord
            .extra
            .insert("allow_all_users".to_string(), serde_json::json!(true));
        allow_all_config
            .platforms
            .insert("discord".to_string(), discord);
        assert!(!gateway_allowlist_startup_would_warn_with_lookup(
            &allow_all_config,
            empty_env
        ));
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
        assert!(seen.contains("nous-api"));
    }

    #[test]
    fn setup_minimax_defaults_use_m3_frontier_model() {
        let providers = setup_provider_defaults();
        let minimax = providers
            .iter()
            .find(|option| option.provider == "minimax")
            .expect("minimax setup option");
        let minimax_cn = providers
            .iter()
            .find(|option| option.provider == "minimax-cn")
            .expect("minimax-cn setup option");

        assert_eq!(minimax.model, "minimax:MiniMax-M3");
        assert_eq!(minimax_cn.model, "minimax-cn:MiniMax-M3");
        assert!(!minimax.model.to_ascii_lowercase().contains("highspeed"));
        assert!(!minimax_cn.model.to_ascii_lowercase().contains("highspeed"));
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
        let idx =
            setup_default_model_pick_index("nous-api", "nous-api:nonexistent/model", &suggested);
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
        assert_eq!(setup_provider_display("nous-api"), "Nous Portal API");
        assert_eq!(setup_provider_env_keys("nous-api"), &["NOUS_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("nous-api"),
            Some(DEFAULT_NOUS_INFERENCE_URL)
        );
        assert_eq!(
            setup_provider_env_keys("kimi-coding"),
            &["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("kimi-coding"),
            Some(provider_profiles::KIMI_CODE_BASE_URL)
        );
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
        assert!(!setup_provider_requires_api_key("bedrock"));
        assert_eq!(setup_provider_display("bedrock"), "AWS Bedrock");
        assert_eq!(
            setup_provider_env_keys("bedrock"),
            &[
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN"
            ]
        );
        assert!(setup_provider_requires_api_key("openai"));
        assert_eq!(setup_provider_display("alibaba"), "Alibaba Cloud DashScope");
        assert_eq!(
            setup_provider_env_keys("google-gemini-cli"),
            &["HERMES_GEMINI_OAUTH_API_KEY"]
        );
        assert_eq!(
            setup_provider_env_keys("copilot"),
            &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
        );
        assert_eq!(
            setup_provider_default_base_url("copilot"),
            Some("https://api.githubcopilot.com")
        );
        assert_eq!(setup_provider_display("gmi"), "GMI Cloud");
        assert_eq!(setup_provider_env_keys("gmi"), &["GMI_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("gmi"),
            Some("https://api.gmi-serving.com/v1")
        );
        assert_eq!(
            setup_provider_display("tencent-tokenhub"),
            "Tencent TokenHub"
        );
        assert_eq!(
            setup_provider_env_keys("tencent-tokenhub"),
            &["TOKENHUB_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("tencent-tokenhub"),
            Some("https://tokenhub.tencentmaas.com/v1")
        );
        assert_eq!(
            setup_provider_default_base_url("ai-gateway"),
            Some("https://ai-gateway.vercel.sh/v1")
        );
        assert_eq!(setup_provider_display("novita"), "NovitaAI");
        assert_eq!(setup_provider_env_keys("novita"), &["NOVITA_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("novita"),
            Some("https://api.novita.ai/openai/v1")
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
        std::env::set_var("NOUS_API_KEY", "env-stale-key");

        hydrate_provider_env_from_vault_for_cli(&cli)
            .await
            .expect("hydrate env");
        assert_eq!(
            std::env::var("NOUS_API_KEY").as_deref(),
            Ok("vault-good-key")
        );

        match previous {
            Some(value) => std::env::set_var("NOUS_API_KEY", value),
            None => std::env::remove_var("NOUS_API_KEY"),
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
    fn provenance_sign_and_verify_round_trip() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");

        let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");
        let verified =
            verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
        assert!(verified.ok, "verification should pass");
        assert_eq!(verified.code, "ok");
        assert!(verified.reason.is_none(), "no reason on success");
    }

    #[test]
    fn provenance_verify_detects_tampered_artifact() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");
        let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

        std::fs::write(&artifact, b"{\"ok\":false}").expect("tamper artifact");

        let verified =
            verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
        assert!(!verified.ok, "tamper must fail");
        assert_eq!(verified.code, "artifact_sha256_mismatch");
        assert_eq!(verified.reason.as_deref(), Some("artifact_sha256 mismatch"));
    }

    #[test]
    fn provenance_verify_detects_signature_mismatch() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");
        let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

        let mut parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("read sidecar"))
                .expect("parse sidecar");
        parsed["signature_hex"] = serde_json::json!("deadbeef");
        std::fs::write(
            &sidecar,
            serde_json::to_string_pretty(&parsed).expect("serialize sidecar"),
        )
        .expect("write tampered sidecar");

        let verified =
            verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
        assert!(!verified.ok, "signature mismatch must fail");
        assert_eq!(verified.code, "signature_mismatch");
        assert_eq!(verified.reason.as_deref(), Some("signature mismatch"));
    }

    #[test]
    fn provenance_verify_detects_missing_sidecar_with_code() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        std::fs::write(&artifact, b"{\"ok\":true}").expect("write artifact");

        let verified = verify_artifact_provenance(&cli, &artifact, None).expect("verify");
        assert!(!verified.ok, "missing sidecar must fail");
        assert_eq!(verified.code, "signature_read_error");
        assert!(verified
            .reason
            .as_deref()
            .unwrap_or("")
            .contains(".sig.json"));
    }

    #[tokio::test]
    async fn rotate_provenance_key_archives_previous_key_and_rekeys() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let old_key = load_or_create_provenance_key(&cli, true).expect("create key");
        run_rotate_provenance_key(cli.clone(), true)
            .await
            .expect("rotate key");
        let new_key = load_or_create_provenance_key(&cli, false).expect("load rotated key");
        assert_ne!(old_key, new_key, "rotation must change active key bytes");

        let auth_dir = provenance_key_path_for_cli(&cli)
            .parent()
            .expect("key path parent")
            .to_path_buf();
        let archived_count = std::fs::read_dir(auth_dir)
            .expect("read auth dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("provenance.key.")
                    && entry.file_name().to_string_lossy().ends_with(".bak")
            })
            .count();
        assert!(archived_count >= 1, "rotation should archive previous key");
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
    fn read_gateway_pid_supports_plain_and_json_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plain = tmp.path().join("plain.pid");
        std::fs::write(&plain, "12345\n").expect("write plain pid");
        assert_eq!(read_gateway_pid(&plain), Some(12345));

        let json = tmp.path().join("json.pid");
        std::fs::write(
            &json,
            serde_json::json!({
                "pid": 23456,
                "kind": "hermes-gateway",
                "argv": ["hermes-gateway"]
            })
            .to_string(),
        )
        .expect("write json pid");
        assert_eq!(read_gateway_pid(&json), Some(23456));

        let invalid = tmp.path().join("invalid.pid");
        std::fs::write(&invalid, "{bad").expect("write invalid pid");
        assert_eq!(read_gateway_pid(&invalid), None);
    }

    #[test]
    fn read_interactive_lock_pid_supports_plain_and_json_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plain = tmp.path().join("interactive.lock");
        std::fs::write(&plain, "12345\n").expect("write plain lock");
        assert_eq!(read_interactive_lock_pid(&plain), Some(12345));

        let json = tmp.path().join("interactive.json");
        std::fs::write(&json, r#"{"pid":23456}"#).expect("write json lock");
        assert_eq!(read_interactive_lock_pid(&json), Some(23456));
    }

    #[test]
    fn query_is_local_slash_command_detects_prefixed_queries() {
        assert!(query_is_local_slash_command("/model list"));
        assert!(query_is_local_slash_command("   /graph status"));
        assert!(!query_is_local_slash_command("hello world"));
    }

    #[test]
    fn interactive_tty_error_is_actionable() {
        let msg = interactive_tty_error_message();
        assert!(msg.contains("requires a terminal"));
        assert!(msg.contains("hermes-ultra setup"));
        assert!(msg.contains("chat --query"));
        assert!(msg.contains("doctor --deep --snapshot --bundle"));
    }

    #[test]
    fn interactive_session_lock_guard_replaces_stale_pid_and_cleans_up() {
        let old_bypass = std::env::var_os(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        std::env::remove_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let lock_path = interactive_lock_path_for_cli(&cli);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir lock parent");
        }
        std::fs::write(&lock_path, "999999").expect("write stale lock");
        let guard = InteractiveSessionLockGuard::acquire(&cli)
            .expect("acquire lock")
            .expect("guard enabled");
        assert_eq!(
            read_interactive_lock_pid(&lock_path),
            Some(std::process::id())
        );
        drop(guard);
        assert!(!lock_path.exists(), "lock file should be removed on drop");
        if let Some(value) = old_bypass {
            std::env::set_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV, value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn interactive_session_lock_guard_rejects_live_pid() {
        let old_bypass = std::env::var_os(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        std::env::remove_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let lock_path = interactive_lock_path_for_cli(&cli);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir lock parent");
        }
        // PID 1 should always be alive on Unix systems.
        std::fs::write(&lock_path, "1").expect("write lock");
        let err = match InteractiveSessionLockGuard::acquire(&cli) {
            Err(err) => err,
            Ok(_) => panic!("must reject live lock holder"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("Another Hermes interactive session is running"));
        assert_eq!(read_interactive_lock_pid(&lock_path), Some(1));
        if let Some(value) = old_bypass {
            std::env::set_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV, value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn parse_pid_snapshot_line_parses_ppid_tty_and_command() {
        let snap = parse_pid_snapshot_line("1 ?? /Users/sheawinkler/.cargo/bin/hermes-agent-ultra")
            .expect("snapshot");
        assert_eq!(snap.ppid, 1);
        assert_eq!(snap.tty, "??");
        assert!(snap.command.contains("hermes-agent-ultra"));
    }

    #[cfg(unix)]
    #[test]
    fn looks_like_interactive_hermes_process_matches_cli_and_not_gateway() {
        assert!(looks_like_interactive_hermes_process(
            "/Users/sheawinkler/.cargo/bin/hermes-agent-ultra"
        ));
        assert!(looks_like_interactive_hermes_process("hermes-ultra"));
        assert!(!looks_like_interactive_hermes_process(
            "/Users/sheawinkler/.cargo/bin/hermes-gateway"
        ));
    }

    #[test]
    fn looks_like_gateway_process_includes_gateway_script_pattern() {
        assert!(looks_like_gateway_process(
            "python -m hermes_cli.main gateway run"
        ));
        assert!(looks_like_gateway_process(
            "python hermes_cli/main.py gateway run"
        ));
        assert!(looks_like_gateway_process("hermes gateway run"));
        assert!(looks_like_gateway_process(
            "hermes-gateway --config ~/.hermes"
        ));
        assert!(looks_like_gateway_process("python gateway/run.py"));
        assert!(!looks_like_gateway_process("python worker.py"));
    }

    #[test]
    fn cleanup_stale_gateway_metadata_removes_pid_and_lock_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid_path = tmp.path().join("gateway.pid");
        let lock_path = gateway_lock_path_for_pid_path(&pid_path);
        std::fs::write(&pid_path, "999999\n").expect("write pid");
        std::fs::write(&lock_path, "{\"pid\":999999}").expect("write lock");

        cleanup_stale_gateway_metadata(&pid_path);
        assert!(!pid_path.exists());
        assert!(!lock_path.exists());
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
        telegram
            .extra
            .insert("webhook_secret".to_string(), serde_json::json!("tg-secret"));
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
        assert!(actions
            .iter()
            .any(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("created")));
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
        let pid_path = gateway_pid_path_for_cli(&cli);
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
    fn resolve_resume_session_file_prefers_latest_modified_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let old = sessions_dir.join("old-session.json");
        let new = sessions_dir.join("new-session.json");
        std::fs::write(&old, r#"{"messages":[{"role":"user","content":"old"}]}"#)
            .expect("write old session");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new, r#"{"messages":[{"role":"user","content":"new"}]}"#)
            .expect("write new session");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
        assert_eq!(resolved, "new-session");
        assert_eq!(path, new);
    }

    #[test]
    fn resolve_resume_session_file_latest_prefers_canonical_session_stem() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let canonical = sessions_dir.join("c0ffee00-0000-4000-8000-000000000001.json");
        std::fs::write(
            &canonical,
            r#"{
  "session_info": {"session_id":"c0ffee00-0000-4000-8000-000000000001","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write canonical");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let named = sessions_dir.join("newest.json");
        std::fs::write(
            &named,
            r#"{
  "session_info": {"session_id":"snap-prune","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"snapshot payload"}]
}"#,
        )
        .expect("write named artifact");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
        assert_eq!(resolved, "c0ffee00-0000-4000-8000-000000000001");
        assert_eq!(path, canonical);
    }

    #[test]
    fn should_resume_fallback_to_fresh_only_for_latest_missing_state() {
        let latest_missing = AgentError::Config("No saved sessions found in /tmp".to_string());
        assert!(should_resume_fallback_to_fresh(None, &latest_missing));
        assert!(should_resume_fallback_to_fresh(
            Some("latest"),
            &latest_missing
        ));
        assert!(!should_resume_fallback_to_fresh(
            Some("abc123"),
            &latest_missing
        ));

        let other_error = AgentError::Config("Session 'abc123' not found".to_string());
        assert!(!should_resume_fallback_to_fresh(None, &other_error));
    }

    #[test]
    fn load_resume_payload_restores_metadata_and_messages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join("abc123.json");
        std::fs::write(
            &session_path,
            r#"{
  "session_info": {
    "session_id": "session-xyz",
    "model": "nous:openai/gpt-5.5-pro",
    "personality": "technical"
  },
  "messages": [
    {"role":"System","content":"[SESSION_OBJECTIVE] Keep context fresh"},
    {"role":"User","content":"hello"},
    {"role":"Assistant","content":"world"}
  ]
}"#,
        )
        .expect("write session");

        let payload = load_resume_payload(&cli, Some("abc123")).expect("load payload");
        assert_eq!(payload.resolved_id, "abc123");
        assert_eq!(payload.session_id, "session-xyz");
        assert_eq!(payload.model.as_deref(), Some("nous:openai/gpt-5.5-pro"));
        assert_eq!(payload.personality.as_deref(), Some("technical"));
        assert_eq!(payload.messages.len(), 3);
        assert!(matches!(
            payload.messages[0].role,
            hermes_core::MessageRole::System
        ));
        assert!(matches!(
            payload.messages[1].role,
            hermes_core::MessageRole::User
        ));
        assert!(matches!(
            payload.messages[2].role,
            hermes_core::MessageRole::Assistant
        ));
    }

    #[test]
    fn load_resume_payload_falls_back_to_legacy_sessions_dir() {
        let _guard = env_lock();
        let prev_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_home = tmp.path().join("fake-home");
        let legacy_sessions = fake_home.join(".hermes").join("sessions");
        std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");
        let legacy_path = legacy_sessions.join("legacy-abc.json");
        std::fs::write(
            &legacy_path,
            r#"{
  "session_info": {
    "session_id": "legacy-session",
    "model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": [
    {"role":"User","content":"from-legacy"}
  ]
}"#,
        )
        .expect("write legacy session");

        std::env::set_var("HOME", &fake_home);
        let state_root = tmp.path().join("ultra-state");
        let cli = cli_for_temp_state_root(&state_root);
        let payload = load_resume_payload(&cli, Some("legacy-abc")).expect("load payload");
        assert_eq!(payload.resolved_id, "legacy-abc");
        assert_eq!(payload.session_id, "legacy-session");
        assert_eq!(payload.messages.len(), 1);
        assert!(payload.source_path.starts_with(&legacy_sessions));

        match prev_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn load_resume_payload_accepts_empty_messages_for_startup_snapshot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join("empty-messages.json");
        std::fs::write(
            &session_path,
            r#"{
  "session_info": {
    "session_id": "empty-messages",
    "model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": []
}"#,
        )
        .expect("write empty session");

        let payload = load_resume_payload(&cli, Some("empty-messages")).expect("load payload");
        assert_eq!(payload.resolved_id, "empty-messages");
        assert_eq!(payload.session_id, "empty-messages");
        assert_eq!(
            payload.model.as_deref(),
            Some("nous:nousresearch/hermes-4-70b")
        );
        assert_eq!(payload.messages.len(), 0);
    }

    #[test]
    fn load_resume_payload_latest_prefers_nonempty_snapshot_over_newer_empty_snapshot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let non_empty = sessions_dir.join("history-real.json");
        std::fs::write(
            &non_empty,
            r#"{
  "session_info": {"session_id":"history-real","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"},{"role":"Assistant","content":"world"}]
}"#,
        )
        .expect("write non-empty session");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let empty_snapshot = sessions_dir.join("startup-empty.json");
        std::fs::write(
            &empty_snapshot,
            r#"{
  "session_info": {"session_id":"startup-empty","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write empty session");

        let payload = load_resume_payload(&cli, None).expect("load payload");
        assert_eq!(payload.resolved_id, "history-real");
        assert_eq!(payload.messages.len(), 2);
        assert_eq!(payload.source_path, non_empty);
    }

    #[test]
    fn load_resume_payload_latest_falls_back_to_legacy_nonempty_when_primary_empty_only() {
        let _guard = env_lock();
        let prev_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_home = tmp.path().join("fake-home");
        let legacy_sessions = fake_home.join(".hermes").join("sessions");
        std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");

        let legacy_non_empty = legacy_sessions.join("legacy-rich.json");
        std::fs::write(
            &legacy_non_empty,
            r#"{
  "session_info": {"session_id":"legacy-rich","model":"nous:nousresearch/hermes-4-70b"},
  "messages":[{"role":"User","content":"from legacy"}]
}"#,
        )
        .expect("write legacy non-empty session");

        std::env::set_var("HOME", &fake_home);
        let state_root = tmp.path().join("ultra-state");
        let cli = cli_for_temp_state_root(&state_root);
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        std::fs::write(
            sessions_dir.join("empty-only.json"),
            r#"{
  "session_info": {"session_id":"empty-only","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write primary empty session");

        let payload = load_resume_payload(&cli, None).expect("load payload");
        assert_eq!(payload.resolved_id, "legacy-rich");
        assert_eq!(payload.messages.len(), 1);
        assert!(payload.source_path.starts_with(&legacy_sessions));

        match prev_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[tokio::test]
    async fn run_dump_writes_real_saved_session_export_with_system_prompt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        std::fs::write(
            sessions_dir.join("abc123.json"),
            r#"{
  "session_info": {
    "session_id": "session-xyz",
    "model": "nous:openai/gpt-5.5",
    "personality": "technical",
    "created_at": "2026-06-05T09:00:00Z"
  },
  "system_prompt": "persisted system prompt",
  "messages": [
    {"role":"User","content":"hello"},
    {"role":"Assistant","content":"world"}
  ]
}"#,
        )
        .expect("write session");

        run_dump(cli, Some("abc123".to_string()), None)
            .await
            .expect("dump session");

        let saved_dir = tmp.path().join("sessions").join("saved");
        let entries = std::fs::read_dir(&saved_dir)
            .expect("saved dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("saved entries");
        assert_eq!(entries.len(), 1);
        let path = entries[0].path();
        assert!(path
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|name| name.starts_with("hermes_conversation_")));

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read dump"))
                .expect("parse dump");
        assert_eq!(doc["session_id"], "session-xyz");
        assert_eq!(doc["resolved_id"], "abc123");
        assert_eq!(doc["model"], "nous:openai/gpt-5.5");
        assert_eq!(doc["personality"], "technical");
        assert_eq!(doc["system_prompt"], "persisted system prompt");
        assert_eq!(doc["session_start"], "2026-06-05T09:00:00Z");
        assert_eq!(doc["messages"].as_array().map(Vec::len), Some(2));
        assert!(doc["source_path"]
            .as_str()
            .is_some_and(|p| p.ends_with("abc123.json")));
    }

    #[test]
    fn route_health_tier_marks_failure_streak_critical() {
        let stats = RouteLearningStatsRecord {
            samples: 8,
            success_rate: 0.61,
            avg_latency_ms: 2200.0,
            consecutive_failures: 6,
            updated_at_unix_ms: 1_700_000_000_000,
        };
        let (tier, reasons, score) = route_health_tier(&stats, route_learning_score(&stats));
        assert_eq!(tier, "critical");
        assert!(reasons.iter().any(|r| r == "failure_streak_critical"));
        assert!(score >= 0.0);
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

    #[test]
    fn parse_simple_env_file_supports_export_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_path = tmp.path().join("route-autotune.env");
        std::fs::write(
            &env_path,
            "# comment\nexport HERMES_SMART_ROUTING_LEARNING_ALPHA=0.240\nHERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS=0.110\n",
        )
        .expect("write env");
        let parsed = parse_simple_env_file(&env_path);
        assert_eq!(
            parsed
                .get("HERMES_SMART_ROUTING_LEARNING_ALPHA")
                .map(String::as_str),
            Some("0.240")
        );
        assert_eq!(
            parsed
                .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
                .map(String::as_str),
            Some("0.110")
        );
    }

    #[test]
    fn apply_route_autotune_env_overrides_sets_missing_keys_only() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "status",
        ]);
        let env_path = route_autotune_env_path_for_cli(&cli);
        if let Some(parent) = env_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(
            &env_path,
            "HERMES_SMART_ROUTING_LEARNING_ALPHA=0.300\nHERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN=0.050\n",
        )
        .expect("write env");

        std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
        std::env::set_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN", "0.999");
        let applied = apply_route_autotune_env_overrides(&cli);
        assert!(applied
            .iter()
            .any(|k| k == "HERMES_SMART_ROUTING_LEARNING_ALPHA"));
        assert_eq!(
            std::env::var("HERMES_SMART_ROUTING_LEARNING_ALPHA").ok(),
            Some("0.300".to_string())
        );
        assert_eq!(
            std::env::var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN").ok(),
            Some("0.999".to_string()),
            "explicit env var should not be overridden"
        );
        std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
        std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN");
    }

    #[test]
    fn build_route_autotune_plan_raises_bias_for_critical_health() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "status",
        ]);
        let entry = RouteHealthEntry {
            key: "openai:gpt-4o".to_string(),
            health_score: 0.2,
            tier: "critical".to_string(),
            reasons: vec!["failure_streak_critical".to_string()],
            stats: RouteLearningStatsRecord {
                samples: 9,
                success_rate: 0.4,
                avg_latency_ms: 5200.0,
                consecutive_failures: 7,
                updated_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            },
        };
        let summary = serde_json::json!({
            "entries": 1,
            "overall": "critical",
            "average_score": 0.2,
            "healthy": 0,
            "watch": 0,
            "degraded": 0,
            "critical": 1
        });
        let plan = build_route_autotune_plan(
            &cli,
            Path::new("/tmp/route-learning.json"),
            Path::new("/tmp/route-health.json"),
            &[entry],
            &summary,
        );
        let cheap_bias = plan
            .overrides
            .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let switch_margin = plan
            .overrides
            .get("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        assert!(cheap_bias >= 0.14);
        assert!(switch_margin >= 0.05);
        assert_eq!(plan.confidence, "low");
    }
}
