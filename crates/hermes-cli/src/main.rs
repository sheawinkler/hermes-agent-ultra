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
    provider_oauth_token_from_auth_state, select_startup_model_with_fallback_and_auth_resolver,
};
use hermes_cli::auth::{
    clear_provider_auth_state, discover_existing_anthropic_oauth, discover_existing_nous_oauth,
    discover_existing_openai_codex_oauth, discover_existing_openai_oauth,
    get_anthropic_oauth_status, get_gemini_oauth_auth_status, get_qwen_auth_status,
    login_anthropic_oauth, login_google_gemini_cli_oauth, login_nous_device_code,
    login_openai_codex_device_code, login_openai_device_code, nous_auth_state_from_runtime_token,
    read_nous_auth_state, read_provider_auth_state, read_valid_nous_auth_state,
    resolve_gemini_oauth_runtime_credentials, resolve_nous_runtime_credentials,
    resolve_qwen_runtime_credentials, save_codex_auth_state, save_nous_auth_state,
    save_openai_auth_state, save_provider_auth_state, AnthropicOAuthLoginOptions,
    CodexDeviceCodeOptions, GeminiOAuthLoginOptions, NousAuthState, NousDeviceCodeOptions,
    NousRuntimeCredentials, ANTHROPIC_OAUTH_CLIENT_ID, ANTHROPIC_OAUTH_TOKEN_URL,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL, DEFAULT_CODEX_BASE_URL,
    DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS, DEFAULT_NOUS_CLIENT_ID, DEFAULT_NOUS_INFERENCE_URL,
    DEFAULT_NOUS_PORTAL_URL, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, QWEN_OAUTH_CLIENT_ID, QWEN_OAUTH_TOKEN_URL,
};
use hermes_cli::cli::{Cli, CliCommand};
use hermes_cli::config_env::hydrate_env_from_config;
use hermes_cli::model_switch::{
    cached_provider_catalog_status, format_stale_auxiliary_warning, normalize_provider_model,
    provider_catalog_entries_for_config, provider_model_ids, provider_picker_description,
    provider_slug_from_provider_model,
};
use hermes_cli::providers::provider_capability_for;
use hermes_cli::runtime_tool_wiring::{
    wire_cron_scheduler_backend, wire_gateway_clarify_backend, wire_gateway_messaging_backend,
};
use hermes_cli::terminal_backend::build_terminal_backend;
use hermes_cli::App;
use hermes_cli_ui::tool_preview::{
    build_gateway_tool_progress_message_with_labels, build_tool_label_from_value, tool_emoji,
};
use hermes_config::{
    gateway_pid_path_in, hermes_home, load_config, load_user_config_file, save_config_yaml,
    set_user_config_value, state_dir, user_config_field_display, validate_config, ConfigError,
    GatewayConfig, PlatformConfig, UnauthorizedDmBehavior,
};
use hermes_core::AgentError;
use hermes_core::ParseMode;
#[cfg(feature = "gateway-telegram")]
use hermes_core::PlatformAdapter;
use hermes_core::{MessageRole, StreamChunk};
use hermes_cron::{
    cron_scheduler_for_data_dir, CronCompletionEvent, CronError, CronRunner, CronScheduler,
    DeliverTarget, FileJobPersistence,
};
use hermes_gateway::gateway::GatewayConfig as RuntimeGatewayConfig;
#[cfg(any(
    feature = "gateway-api-server",
    feature = "gateway-dingtalk",
    feature = "gateway-ntfy",
    feature = "gateway-telegram",
    feature = "gateway-webhook",
    feature = "gateway-wecom-callback",
    feature = "gateway-weixin"
))]
use hermes_gateway::gateway::IncomingMessage as GatewayIncomingMessage;
use hermes_gateway::gateway::{GroupAccessMode, PlatformAccessPolicy};
use hermes_gateway::hooks::HookRegistry;
#[cfg(feature = "gateway-api-server")]
use hermes_gateway::platforms::api_server::{ApiInboundRequest, ApiServerAdapter, ApiServerConfig};
#[cfg(feature = "gateway-bluebubbles")]
use hermes_gateway::platforms::bluebubbles::{BlueBubblesAdapter, BlueBubblesConfig};
#[cfg(feature = "gateway-dingtalk")]
use hermes_gateway::platforms::dingtalk::{DingTalkAdapter, DingTalkConfig};
#[cfg(feature = "gateway-discord")]
use hermes_gateway::platforms::discord::{
    DiscordAdapter, DiscordChannelControls, DiscordChannelSkillBinding, DiscordConfig,
};
#[cfg(feature = "gateway-email")]
use hermes_gateway::platforms::email::{EmailAdapter, EmailConfig};
#[cfg(feature = "gateway-feishu")]
use hermes_gateway::platforms::feishu::{FeishuAdapter, FeishuConfig};
#[cfg(feature = "gateway-homeassistant")]
use hermes_gateway::platforms::homeassistant::{HomeAssistantAdapter, HomeAssistantConfig};
#[cfg(feature = "gateway-matrix")]
use hermes_gateway::platforms::matrix::{MatrixAdapter, MatrixConfig};
#[cfg(feature = "gateway-mattermost")]
use hermes_gateway::platforms::mattermost::{MattermostAdapter, MattermostConfig};
#[cfg(feature = "gateway-ntfy")]
use hermes_gateway::platforms::ntfy::{NtfyAdapter, NtfyConfig};
#[cfg(feature = "gateway-qqbot")]
use hermes_gateway::platforms::qqbot::{QqBotAdapter, QqBotConfig};
#[cfg(feature = "gateway-signal")]
use hermes_gateway::platforms::signal::{SignalAdapter, SignalConfig};
#[cfg(feature = "gateway-slack")]
use hermes_gateway::platforms::slack::{SlackAdapter, SlackConfig};
#[cfg(feature = "gateway-sms")]
use hermes_gateway::platforms::sms::{SmsAdapter, SmsConfig};
#[cfg(feature = "gateway-telegram")]
use hermes_gateway::platforms::telegram::{
    IncomingMessage as TelegramIncomingMessage, PollResult as TelegramPollResult, TelegramAdapter,
    TelegramConfig, TelegramTextBatcher,
};
#[cfg(feature = "gateway-webhook")]
use hermes_gateway::platforms::webhook::{WebhookAdapter, WebhookConfig, WebhookPayload};
#[cfg(feature = "gateway-wecom")]
use hermes_gateway::platforms::wecom::{WeComAdapter, WeComConfig};
#[cfg(feature = "gateway-wecom-callback")]
use hermes_gateway::platforms::wecom_callback::{
    WeComCallbackAdapter, WeComCallbackApp, WeComCallbackConfig,
};
#[cfg(feature = "gateway-weixin")]
use hermes_gateway::platforms::weixin::{WeChatAdapter, WeixinConfig};
#[cfg(feature = "gateway-whatsapp")]
use hermes_gateway::platforms::whatsapp::{WhatsAppAdapter, WhatsAppConfig};
use hermes_gateway::tool_backends::ClarifyDispatcher;
use hermes_gateway::{
    ActiveSessionControl, DmManager, Gateway, GatewayRuntimeContext, SessionManager,
};
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_telemetry::init_telemetry_from_env;
use hermes_tool_planning::{resolve_platform_tool_schemas, tool_definition_summary};
use hermes_tools::{default_tool_policy_counters_path, load_tool_policy_counters, ToolRegistry};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
#[cfg(any(
    feature = "gateway-api-server",
    feature = "gateway-dingtalk",
    feature = "gateway-ntfy",
    feature = "gateway-webhook",
    feature = "gateway-wecom-callback",
    feature = "gateway-weixin"
))]
use tokio::sync::mpsc;

// Keep these includes in the original item order. This first split is layout-only:
// every item remains in the binary crate-root namespace while the entrypoint is
// decomposed by subsystem for reviewability and follow-up module extraction.
include!("main/startup_helpers.rs");
include!("main/entrypoint.rs");
include!("main/interactive_resume.rs");
include!("main/tools_config.rs");
include!("main/gateway_service.rs");
include!("main/gateway_command.rs");
include!("main/gateway_access.rs");
include!("main/gateway_adapters.rs");
include!("main/auth_providers.rs");
include!("main/auth_commands.rs");
include!("main/cron_webhook.rs");
include!("main/setup_flow.rs");
include!("main/doctor_routes.rs");
include!("main/status_debug_profile.rs");
include!("main/tests.rs");
