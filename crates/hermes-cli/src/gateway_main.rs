//! Gateway setup, configuration, and adapter registration.
//!
//! Extracted from `main.rs` — this module owns the gateway platform configuration
//! logic, adapters registration, agent caching, and related helpers.
//!
//! Functions in this module access their parent (`super::`) for a few items:
//! - `crate::auth_main::run_auth`
//! - `crate::prompt::prompt_line`
//!
//! Inbound message loops live in `crate::gateway_runtime`.

use crate::app::{async_tool_dispatch_for, build_agent_config, build_provider};
use crate::cli::Cli;
use crate::whatsapp_wizard;
use hermes_agent::AgentLoop;
use hermes_agent::session_persistence::SessionPersistence;
use hermes_config::{
    GatewayConfig, PlatformConfig, UnauthorizedDmBehavior, hermes_home, load_user_config_file,
};
use hermes_core::AgentError;
use hermes_core::PlatformAdapter;
use hermes_gateway::gateway::IncomingMessage as GatewayIncomingMessage;
use hermes_gateway::gateway::{DmAccessMode, GroupAccessMode, PlatformAccessPolicy};
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
use hermes_gateway::platforms::whatsapp::{WhatsAppAdapter, WhatsAppConfig};
use hermes_gateway::{DmManager, Gateway, GatewayRuntimeContext, SessionManager};
use hermes_tools::ToolRegistry;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Gateway platform catalog
// ---------------------------------------------------------------------------

pub(crate) struct GatewayPlatformEntry {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) emoji: &'static str,
}

pub(crate) const GATEWAY_PLATFORM_CATALOG: &[GatewayPlatformEntry] = &[
    GatewayPlatformEntry {
        key: "telegram",
        label: "Telegram",
        emoji: "📱",
    },
    GatewayPlatformEntry {
        key: "discord",
        label: "Discord",
        emoji: "💬",
    },
    GatewayPlatformEntry {
        key: "slack",
        label: "Slack",
        emoji: "💼",
    },
    GatewayPlatformEntry {
        key: "mattermost",
        label: "Mattermost",
        emoji: "💬",
    },
    GatewayPlatformEntry {
        key: "whatsapp",
        label: "WhatsApp",
        emoji: "📲",
    },
    GatewayPlatformEntry {
        key: "signal",
        label: "Signal",
        emoji: "📡",
    },
    GatewayPlatformEntry {
        key: "email",
        label: "Email",
        emoji: "📧",
    },
    GatewayPlatformEntry {
        key: "sms",
        label: "SMS (Twilio)",
        emoji: "📱",
    },
    GatewayPlatformEntry {
        key: "dingtalk",
        label: "DingTalk",
        emoji: "💬",
    },
    GatewayPlatformEntry {
        key: "feishu",
        label: "Feishu / Lark",
        emoji: "🪽",
    },
    GatewayPlatformEntry {
        key: "wecom",
        label: "WeCom (Enterprise WeChat)",
        emoji: "💬",
    },
    GatewayPlatformEntry {
        key: "wecom_callback",
        label: "WeCom Callback (Self-Built App)",
        emoji: "💬",
    },
    GatewayPlatformEntry {
        key: "weixin",
        label: "Weixin / WeChat",
        emoji: "💬",
    },
    GatewayPlatformEntry {
        key: "bluebubbles",
        label: "BlueBubbles (iMessage)",
        emoji: "💬",
    },
    GatewayPlatformEntry {
        key: "qqbot",
        label: "QQ Bot",
        emoji: "🐧",
    },
    GatewayPlatformEntry {
        key: "matrix",
        label: "Matrix",
        emoji: "🔗",
    },
    GatewayPlatformEntry {
        key: "homeassistant",
        label: "Home Assistant",
        emoji: "🏠",
    },
    GatewayPlatformEntry {
        key: "webhook",
        label: "Webhook",
        emoji: "🪝",
    },
    GatewayPlatformEntry {
        key: "api_server",
        label: "API Server",
        emoji: "🌐",
    },
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn extra_string(platform_cfg: &PlatformConfig, key: &str) -> Option<String> {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub(crate) fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub(crate) fn extra_bool(platform_cfg: &PlatformConfig, key: &str, default: bool) -> bool {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

pub(crate) fn extra_u16(platform_cfg: &PlatformConfig, key: &str, default: u16) -> u16 {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(default)
}

pub(crate) fn extra_bool_loose(platform_cfg: &PlatformConfig, key: &str) -> Option<bool> {
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

pub(crate) fn platform_token_or_extra(platform_cfg: &PlatformConfig) -> Option<String> {
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

pub(crate) fn matrix_home_room_for_platform(platform_cfg: &PlatformConfig) -> Option<String> {
    extra_string(platform_cfg, "room_id")
        .or_else(|| extra_string(platform_cfg, "home_room"))
        .or_else(|| env_string("MATRIX_HOME_ROOM"))
}

pub(crate) fn default_platform_dm_policy(platform: &str) -> &'static str {
    match platform.trim().to_ascii_lowercase().as_str() {
        "wecom" | "weixin" | "qqbot" => "open",
        _ => "pairing",
    }
}

pub(crate) fn platform_dm_policy(platform: &str, platform_cfg: &PlatformConfig) -> String {
    if let Some(policy) = extra_string(platform_cfg, "dm_policy") {
        return policy.to_ascii_lowercase();
    }
    if let Some(nested) = platform_cfg.extra.get("extra") {
        if let Some(policy) = nested.get("dm_policy").and_then(|v| v.as_str()) {
            let trimmed = policy.trim();
            if !trimmed.is_empty() {
                return trimmed.to_ascii_lowercase();
            }
        }
    }
    let env_key = match platform.trim().to_ascii_lowercase().as_str() {
        "wecom" => Some("WECOM_DM_POLICY"),
        "weixin" => Some("WEIXIN_DM_POLICY"),
        _ => None,
    };
    if let Some(key) = env_key {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_ascii_lowercase();
            }
        }
    }
    default_platform_dm_policy(platform).to_string()
}

pub(crate) fn parse_dm_access_mode(platform: &str, platform_cfg: &PlatformConfig) -> DmAccessMode {
    match platform_dm_policy(platform, platform_cfg).as_str() {
        "open" => DmAccessMode::Open,
        "disabled" => DmAccessMode::Disabled,
        "allowlist" => DmAccessMode::Allowlist,
        "pairing" | "pair" => DmAccessMode::Pairing,
        _ => {
            if default_platform_dm_policy(platform) == "open" {
                DmAccessMode::Open
            } else {
                DmAccessMode::Pairing
            }
        }
    }
}

pub(crate) fn platform_dm_is_open(platform: &str, platform_cfg: &PlatformConfig) -> bool {
    platform_dm_policy(platform, platform_cfg) == "open"
}

pub(crate) fn parse_csv_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub(crate) fn set_extra_string_if_nonempty(platform: &mut PlatformConfig, key: &str, value: &str) {
    let v = value.trim();
    if !v.is_empty() {
        platform
            .extra
            .insert(key.to_string(), serde_json::Value::String(v.to_string()));
    }
}

pub async fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool, AgentError> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    let ans = crate::prompt::prompt_line(format!("{question} {hint}: ")).await?;
    if ans.trim().is_empty() {
        return Ok(default_yes);
    }
    let v = ans.trim().to_ascii_lowercase();
    Ok(matches!(v.as_str(), "y" | "yes" | "1" | "true" | "on"))
}

pub(crate) fn parse_group_access_mode(platform_cfg: &PlatformConfig) -> GroupAccessMode {
    let explicit = extra_string(platform_cfg, "group_policy")
        .or_else(|| extra_string(platform_cfg, "group_access"));
    if let Some(policy) = explicit {
        match policy.trim().to_ascii_lowercase().as_str() {
            "disabled" | "deny" | "off" | "none" => return GroupAccessMode::Disabled,
            "allowlist" | "restricted" | "whitelist" => return GroupAccessMode::Allowlist,
            "open" | "all" | "enabled" => return GroupAccessMode::Open,
            _ => {}
        }
    }
    if !platform_cfg.allowed_users.is_empty() || !platform_cfg.admin_users.is_empty() {
        GroupAccessMode::Allowlist
    } else {
        GroupAccessMode::Open
    }
}

/// Resolve Telegram bot token during `hermes gateway setup` (always shows a wizard step).
pub(crate) async fn resolve_telegram_bot_token_for_gateway_setup(
    disk: &GatewayConfig,
) -> Result<String, AgentError> {
    let config_token = disk
        .platforms
        .get("telegram")
        .and_then(platform_token_or_extra);
    let env_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    println!("Create a bot with @BotFather on Telegram (/newbot), then paste the HTTP API token.");

    let prompt = if config_token.is_some() {
        "Telegram bot token (from @BotFather; Enter to keep the token already in config): "
    } else if env_token.is_some() {
        "Telegram bot token (from @BotFather; Enter to use TELEGRAM_BOT_TOKEN from .env): "
    } else {
        "Telegram bot token (from @BotFather): "
    };

    let entered = crate::prompt::prompt_line(prompt).await?;
    let trimmed = entered.trim();
    if !trimmed.is_empty() {
        return Ok(trimmed.to_string());
    }
    if let Some(token) = config_token {
        println!("Keeping existing Telegram bot token from config.");
        return Ok(token);
    }
    if let Some(token) = env_token {
        println!("Using Telegram bot token from TELEGRAM_BOT_TOKEN.");
        return Ok(token);
    }
    Err(AgentError::Config(
        "Telegram bot token is required (from @BotFather, or set TELEGRAM_BOT_TOKEN in ~/.hermes/.env)"
            .into(),
    ))
}

// ---------------------------------------------------------------------------
// Gateway config building helpers
// ---------------------------------------------------------------------------

pub(crate) fn build_telegram_config(
    platform_cfg: &PlatformConfig,
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

pub(crate) fn build_api_server_config(platform_cfg: &PlatformConfig) -> ApiServerConfig {
    ApiServerConfig {
        host: extra_string(platform_cfg, "host").unwrap_or_else(|| "127.0.0.1".to_string()),
        port: extra_u16(platform_cfg, "port", 8090),
        auth_token: platform_token_or_extra(platform_cfg)
            .or_else(|| extra_string(platform_cfg, "auth_token")),
    }
}

pub(crate) fn build_webhook_config(platform_cfg: &PlatformConfig, secret: String) -> WebhookConfig {
    WebhookConfig {
        port: extra_u16(platform_cfg, "port", 9000),
        path: extra_string(platform_cfg, "path").unwrap_or_else(|| "/webhook".to_string()),
        secret,
    }
}

pub(crate) fn apply_telegram_allowlists(platform: &mut PlatformConfig, allowed_users: &[String]) {
    if allowed_users.is_empty() {
        return;
    }
    platform.allowed_users = allowed_users.to_vec();
    platform.extra.insert(
        "allow_from".to_string(),
        serde_json::Value::Array(
            allowed_users
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    platform
        .extra
        .insert("dm_policy".to_string(), serde_json::json!("allowlist"));
}

pub(crate) fn gateway_requirement_issues(config: &GatewayConfig) -> Vec<String> {
    hermes_gateway::gateway_requirement_issues(config)
}

// ---------------------------------------------------------------------------
// DM manager and access policies
// ---------------------------------------------------------------------------

pub(crate) fn build_gateway_dm_manager(config: &GatewayConfig) -> DmManager {
    let enabled: Vec<(&String, &PlatformConfig)> = config
        .platforms
        .iter()
        .filter(|(_, cfg)| cfg.enabled)
        .collect();

    let mut dm_manager = if enabled.is_empty() {
        DmManager::with_pair_behavior()
    } else if enabled
        .iter()
        .all(|(name, cfg)| platform_dm_is_open(name, cfg))
    {
        DmManager::with_open_behavior()
    } else if enabled
        .iter()
        .any(|(_, cfg)| cfg.unauthorized_dm_behavior == UnauthorizedDmBehavior::Ignore)
    {
        DmManager::with_ignore_behavior()
    } else {
        DmManager::with_pair_behavior()
    };

    for (_, platform_cfg) in &enabled {
        for user in &platform_cfg.allowed_users {
            let trimmed = user.trim();
            if !trimmed.is_empty() {
                dm_manager.authorize_user(trimmed.to_string());
            }
        }
        for admin in &platform_cfg.admin_users {
            let trimmed = admin.trim();
            if !trimmed.is_empty() {
                dm_manager.add_admin(trimmed.to_string());
            }
        }
        if let Some(list) = platform_cfg
            .extra
            .get("allow_from")
            .or_else(|| platform_cfg.extra.get("allowFrom"))
        {
            if let Ok(users) = serde_json::from_value::<Vec<String>>(list.clone()) {
                for user in users {
                    let trimmed = user.trim();
                    if !trimmed.is_empty() && trimmed != "*" {
                        dm_manager.authorize_user(trimmed.to_string());
                    }
                }
            }
        }
    }
    dm_manager
}

pub(crate) fn build_gateway_platform_access_policies(
    config: &GatewayConfig,
) -> HashMap<String, PlatformAccessPolicy> {
    let mut policies = HashMap::new();
    for (platform, platform_cfg) in config.platforms.iter().filter(|(_, cfg)| cfg.enabled) {
        let mut allowed_users = HashSet::new();
        let mut admin_users = HashSet::new();
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

        let group_mode = parse_group_access_mode(platform_cfg);
        let mut allowed_roles = HashSet::new();
        if platform.eq_ignore_ascii_case("discord") {
            if let Ok(env_roles) = std::env::var("DISCORD_ALLOWED_ROLES") {
                for role in env_roles.split(',') {
                    let trimmed = role.trim();
                    if !trimmed.is_empty() {
                        allowed_roles.insert(trimmed.to_string());
                    }
                }
            }
            if let Some(roles_val) = platform_cfg.extra.get("allowed_roles") {
                if let Some(arr) = roles_val.as_array() {
                    for role in arr {
                        if let Some(s) = role.as_str() {
                            let trimmed = s.trim();
                            if !trimmed.is_empty() {
                                allowed_roles.insert(trimmed.to_string());
                            }
                        }
                    }
                } else if let Some(s) = roles_val.as_str() {
                    for role in s.split(',') {
                        let trimmed = role.trim();
                        if !trimmed.is_empty() {
                            allowed_roles.insert(trimmed.to_string());
                        }
                    }
                }
            }
        }
        let has_allowlist =
            !allowed_users.is_empty() || !admin_users.is_empty() || !allowed_roles.is_empty();
        let slash_requires_allowlist = extra_bool_loose(platform_cfg, "slash_requires_allowlist")
            .or_else(|| extra_bool_loose(platform_cfg, "require_allowlist_for_slash"))
            .unwrap_or_else(|| platform.eq_ignore_ascii_case("discord") && has_allowlist);

        let mut dm_mode = parse_dm_access_mode(platform, platform_cfg);
        if platform.eq_ignore_ascii_case("whatsapp") {
            let wa_cfg = WhatsAppConfig::from_platform_config(platform_cfg);
            if wa_cfg.self_chat_dm_policy_open() {
                dm_mode = DmAccessMode::Open;
            }
        }
        if dm_mode == DmAccessMode::Pairing
            && platform.eq_ignore_ascii_case("discord")
            && extra_string(platform_cfg, "dm_policy").is_none()
            && (!platform_cfg.allowed_users.is_empty() || !platform_cfg.admin_users.is_empty())
        {
            dm_mode = DmAccessMode::Allowlist;
        }
        if dm_mode == DmAccessMode::Allowlist {
            let allow_from = platform_cfg
                .extra
                .get("allow_from")
                .or_else(|| platform_cfg.extra.get("allowFrom"))
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default();
            for user in allow_from {
                let trimmed = user.trim();
                if !trimmed.is_empty() && trimmed != "*" {
                    allowed_users.insert(trimmed.to_string());
                }
            }
            if allowed_users.is_empty() && admin_users.is_empty() {
                dm_mode = DmAccessMode::Pairing;
            }
        }

        policies.insert(
            platform.to_ascii_lowercase(),
            PlatformAccessPolicy {
                allowed_users,
                admin_users,
                allowed_roles,
                group_mode,
                slash_requires_allowlist,
                dm_mode,
            },
        );
    }
    policies
}

// ---------------------------------------------------------------------------
// Session management and DB maintenance
// ---------------------------------------------------------------------------

pub(crate) fn run_sessions_db_auto_maintenance(config: &GatewayConfig) {
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

pub(crate) fn gateway_session_manager_with_persistence(config: &GatewayConfig) -> SessionManager {
    let home = config
        .home_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(hermes_home);
    let sp = Arc::new(SessionPersistence::new(&home));
    if let Err(err) = sp.ensure_db() {
        tracing::debug!(
            "sessions db init skipped for gateway history hydration: {}",
            err
        );
    }
    let group_sessions_per_user = config
        .platforms
        .values()
        .any(|p| p.enabled && p.group_sessions_per_user);
    let sp_rotator = sp.clone();
    SessionManager::with_group_isolation(config.session.clone(), group_sessions_per_user)
        .with_history_loader(move |session_key| {
            let session_id = match sp.get_indexed_session_id(session_key) {
                Ok(Some(uuid)) => {
                    return match sp.load_session(&uuid) {
                        Ok(msgs) => (msgs, Some(uuid)),
                        Err(err) => {
                            tracing::debug!(
                                session_key = %session_key,
                                "gateway history hydration skipped (uuid): {}",
                                err
                            );
                            (Vec::new(), None)
                        }
                    };
                }
                _ => session_key,
            };
            match sp.load_session(session_id) {
                Ok(messages) => (messages, None),
                Err(err) => {
                    tracing::debug!(
                        session_key = %session_key,
                        "gateway history hydration skipped: {}",
                        err
                    );
                    (Vec::new(), None)
                }
            }
        })
        .with_session_id_rotator(move |session_key, new_uuid| {
            if let Err(err) = sp_rotator.upsert_session_index(session_key, new_uuid) {
                tracing::warn!(
                    session_key = %session_key,
                    "gateway session_id rotation persist failed: {}",
                    err
                );
            }
        })
}

// ---------------------------------------------------------------------------
// Agent cache
// ---------------------------------------------------------------------------

pub(crate) const GATEWAY_AGENT_CACHE_MAX_SIZE: usize = 128;
pub(crate) const GATEWAY_AGENT_CACHE_IDLE_TTL: Duration = Duration::from_secs(3600);

pub(crate) struct GatewayAgentCacheEntry {
    signature: String,
    agent: Arc<tokio::sync::Mutex<AgentLoop>>,
    last_used: Instant,
}

pub(crate) type GatewayAgentCache =
    Arc<tokio::sync::Mutex<HashMap<String, GatewayAgentCacheEntry>>>;

pub(crate) fn gateway_agent_signature(
    config: &GatewayConfig,
    ctx: &GatewayRuntimeContext,
) -> String {
    let effective_model =
        resolve_model_for_gateway(config.model.as_deref().unwrap_or("gpt-4o"), ctx);
    let home = ctx
        .home
        .as_deref()
        .or(config.home_dir.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let effective_session_id = if !ctx.session_id.trim().is_empty() {
        ctx.session_id.as_str()
    } else {
        ctx.session_key.as_str()
    };
    let material = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        effective_model,
        ctx.platform,
        ctx.provider.as_deref().unwrap_or(""),
        ctx.personality.as_deref().unwrap_or(""),
        ctx.service_tier.as_deref().unwrap_or(""),
        effective_session_id,
        home,
        ctx.user_id,
    );
    hex::encode(Sha256::digest(material.as_bytes()))
}

pub(crate) fn prune_gateway_agent_cache(cache: &mut HashMap<String, GatewayAgentCacheEntry>) {
    let now = Instant::now();
    cache.retain(|_, entry| now.duration_since(entry.last_used) <= GATEWAY_AGENT_CACHE_IDLE_TTL);
    if cache.len() <= GATEWAY_AGENT_CACHE_MAX_SIZE {
        return;
    }
    let mut entries: Vec<(String, Instant)> = cache
        .iter()
        .map(|(k, v)| (k.clone(), v.last_used))
        .collect();
    entries.sort_by_key(|(_, used)| *used);
    let overflow = cache.len().saturating_sub(GATEWAY_AGENT_CACHE_MAX_SIZE);
    for (key, _) in entries.into_iter().take(overflow) {
        cache.remove(&key);
    }
}

pub(crate) async fn get_or_build_gateway_cached_agent(
    cache: &GatewayAgentCache,
    config: &GatewayConfig,
    ctx: &GatewayRuntimeContext,
    agent_tools: Arc<hermes_agent::agent_loop::ToolRegistry>,
    runtime_tools: Arc<ToolRegistry>,
) -> Arc<tokio::sync::Mutex<AgentLoop>> {
    let signature = gateway_agent_signature(config, ctx);
    let session_key = ctx.session_key.clone();
    {
        let mut guard = cache.lock().await;
        if let Some(entry) = guard.get_mut(&session_key) {
            if entry.signature == signature {
                entry.last_used = Instant::now();
                tracing::debug!(
                    session_key = %session_key,
                    "gateway agent cache hit"
                );
                return entry.agent.clone();
            }
        }
    }
    let build_start = Instant::now();
    tracing::info!(
        session_key = %session_key,
        "gateway agent cache miss; building agent"
    );
    let built = Arc::new(tokio::sync::Mutex::new(build_agent_for_gateway_context(
        config,
        ctx,
        agent_tools,
        runtime_tools,
    )));
    tracing::info!(
        session_key = %session_key,
        elapsed_ms = build_start.elapsed().as_millis() as u64,
        "gateway agent built"
    );
    let mut guard = cache.lock().await;
    if let Some(entry) = guard.get_mut(&session_key) {
        if entry.signature == signature {
            entry.last_used = Instant::now();
            return entry.agent.clone();
        }
    }
    guard.insert(
        session_key,
        GatewayAgentCacheEntry {
            signature,
            agent: built.clone(),
            last_used: Instant::now(),
        },
    );
    prune_gateway_agent_cache(&mut guard);
    built
}

pub(crate) fn truncate_hook_tool_result(result: &str) -> String {
    let trimmed = result.trim();
    if trimmed.chars().count() <= 240 {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(240).collect();
    format!("{prefix}...")
}

// ---------------------------------------------------------------------------
// Gateway context & agent builder
// ---------------------------------------------------------------------------

pub(crate) fn resolve_model_for_gateway(
    default_model: &str,
    ctx: &GatewayRuntimeContext,
) -> String {
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

pub(crate) fn build_agent_for_gateway_context(
    config: &GatewayConfig,
    ctx: &GatewayRuntimeContext,
    agent_tools: Arc<hermes_agent::agent_loop::ToolRegistry>,
    runtime_tools: Arc<ToolRegistry>,
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
    // Use the rotatable session_id (UUID after /new, session_key before).
    let effective_session_id = if !ctx.session_id.trim().is_empty() {
        ctx.session_id.clone()
    } else {
        ctx.session_key.clone()
    };
    if !effective_session_id.trim().is_empty() {
        agent_config.session_id = Some(effective_session_id.clone());
    }
    let home = ctx
        .home
        .as_deref()
        .or(config.home_dir.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(h) = home {
        agent_config.hermes_home = Some(h.to_string());
        let _ = AgentLoop::hydrate_stored_system_prompt_from_hermes_home(
            &mut agent_config,
            Path::new(h),
        );
    }
    if !effective_session_id.trim().is_empty() {
        let hermes_home = home
            .map(Path::new)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(hermes_config::hermes_home);
        hermes_agent::work_session::touch_active_session(&hermes_home, &effective_session_id);
    }
    hermes_agent::attach_agent_runtime(
        AgentLoop::new(agent_config, agent_tools, provider)
            .with_async_tool_dispatch(async_tool_dispatch_for(runtime_tools.clone()))
            .with_synced_tools_registry(runtime_tools),
    )
}

// ---------------------------------------------------------------------------
// Platform configuration wizards
// ---------------------------------------------------------------------------

pub(crate) async fn configure_platform_basic_prompts(
    disk: &mut GatewayConfig,
    key: &str,
) -> Result<(), AgentError> {
    let p = disk
        .platforms
        .entry(key.to_string())
        .or_insert_with(PlatformConfig::default);
    p.enabled = true;

    match key {
        "discord" => {
            let token = crate::prompt::prompt_line("Discord bot token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let app_id = crate::prompt::prompt_line("Discord application_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "application_id", &app_id);
            let allowed =
                crate::prompt::prompt_line("Discord allowed users (comma-separated, optional): ")
                    .await?;
            if !allowed.trim().is_empty() {
                p.allowed_users = parse_csv_list(&allowed);
            }
            let home = crate::prompt::prompt_line("Discord home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "slack" => {
            let token = crate::prompt::prompt_line("Slack bot token (xoxb-...): ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let app_token =
                crate::prompt::prompt_line("Slack app token (xapp-..., optional): ").await?;
            set_extra_string_if_nonempty(p, "app_token", &app_token);
            let socket_mode = prompt_yes_no("Slack use socket_mode?", true).await?;
            p.extra.insert(
                "socket_mode".to_string(),
                serde_json::Value::Bool(socket_mode),
            );
        }
        "matrix" => {
            let homeserver =
                crate::prompt::prompt_line("Matrix homeserver_url (e.g. https://matrix.org): ")
                    .await?;
            set_extra_string_if_nonempty(p, "homeserver_url", &homeserver);
            let user_id =
                crate::prompt::prompt_line("Matrix user_id (e.g. @bot:matrix.org): ").await?;
            set_extra_string_if_nonempty(p, "user_id", &user_id);
            let token = crate::prompt::prompt_line("Matrix access token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let room = crate::prompt::prompt_line("Matrix home room_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "room_id", &room);
        }
        "mattermost" => {
            let server_url = crate::prompt::prompt_line("Mattermost server_url: ").await?;
            set_extra_string_if_nonempty(p, "server_url", &server_url);
            let token = crate::prompt::prompt_line("Mattermost bot token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let team_id = crate::prompt::prompt_line("Mattermost team_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "team_id", &team_id);
            let home = crate::prompt::prompt_line("Mattermost home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "signal" => {
            let account =
                crate::prompt::prompt_line("Signal phone_number/account (e.g. +15551234567): ")
                    .await?;
            set_extra_string_if_nonempty(p, "phone_number", &account);
            let api_url =
                crate::prompt::prompt_line("Signal api_url (default http://localhost:8080): ")
                    .await?;
            set_extra_string_if_nonempty(p, "api_url", &api_url);
        }
        "dingtalk" => {
            let client_id = crate::prompt::prompt_line("DingTalk client_id/appkey: ").await?;
            set_extra_string_if_nonempty(p, "client_id", &client_id);
            let client_secret = crate::prompt::prompt_line("DingTalk client_secret: ").await?;
            set_extra_string_if_nonempty(p, "client_secret", &client_secret);
        }
        "feishu" => {
            let app_id = crate::prompt::prompt_line("Feishu/Lark app_id: ").await?;
            set_extra_string_if_nonempty(p, "app_id", &app_id);
            let app_secret = crate::prompt::prompt_line("Feishu/Lark app_secret: ").await?;
            set_extra_string_if_nonempty(p, "app_secret", &app_secret);
            let verify =
                crate::prompt::prompt_line("Feishu verification_token (optional): ").await?;
            set_extra_string_if_nonempty(p, "verification_token", &verify);
            let encrypt_key = crate::prompt::prompt_line("Feishu encrypt_key (optional): ").await?;
            set_extra_string_if_nonempty(p, "encrypt_key", &encrypt_key);
        }
        "wecom" => {
            let bot_id = crate::prompt::prompt_line("WeCom AI Bot bot_id (WECOM_BOT_ID): ").await?;
            set_extra_string_if_nonempty(p, "bot_id", &bot_id);
            let secret = crate::prompt::prompt_line("WeCom AI Bot secret (WECOM_SECRET): ").await?;
            set_extra_string_if_nonempty(p, "secret", &secret);
            let ws = crate::prompt::prompt_line(
                "WeCom websocket_url (default wss://openws.work.weixin.qq.com): ",
            )
            .await?;
            if !ws.trim().is_empty() {
                set_extra_string_if_nonempty(p, "websocket_url", &ws);
            }
        }
        "wecom_callback" => {
            let corp_id = crate::prompt::prompt_line("WeCom callback corp_id: ").await?;
            set_extra_string_if_nonempty(p, "corp_id", &corp_id);
            let corp_secret = crate::prompt::prompt_line("WeCom callback corp_secret: ").await?;
            set_extra_string_if_nonempty(p, "corp_secret", &corp_secret);
            let agent_id = crate::prompt::prompt_line("WeCom callback agent_id: ").await?;
            set_extra_string_if_nonempty(p, "agent_id", &agent_id);
            let token = crate::prompt::prompt_line("WeCom callback token: ").await?;
            set_extra_string_if_nonempty(p, "token", &token);
            let aes = crate::prompt::prompt_line("WeCom callback encoding_aes_key: ").await?;
            set_extra_string_if_nonempty(p, "encoding_aes_key", &aes);
            let host =
                crate::prompt::prompt_line("WeCom callback host (default 0.0.0.0): ").await?;
            set_extra_string_if_nonempty(p, "host", &host);
            let port = crate::prompt::prompt_line("WeCom callback port (default 8645): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let path =
                crate::prompt::prompt_line("WeCom callback path (default /wecom/callback): ")
                    .await?;
            set_extra_string_if_nonempty(p, "path", &path);
        }
        "qqbot" => {
            let app_id = crate::prompt::prompt_line("QQBot app_id: ").await?;
            set_extra_string_if_nonempty(p, "app_id", &app_id);
            let secret = crate::prompt::prompt_line("QQBot client_secret: ").await?;
            set_extra_string_if_nonempty(p, "client_secret", &secret);
            let markdown = prompt_yes_no("QQBot markdown_support?", true).await?;
            p.extra.insert(
                "markdown_support".to_string(),
                serde_json::Value::Bool(markdown),
            );
        }
        "bluebubbles" => {
            let server_url = crate::prompt::prompt_line("BlueBubbles server_url: ").await?;
            set_extra_string_if_nonempty(p, "server_url", &server_url);
            let password = crate::prompt::prompt_line("BlueBubbles password: ").await?;
            set_extra_string_if_nonempty(p, "password", &password);
        }
        "email" => {
            let username = crate::prompt::prompt_line("Email username/address: ").await?;
            set_extra_string_if_nonempty(p, "username", &username);
            let password = crate::prompt::prompt_line("Email password/app password: ").await?;
            set_extra_string_if_nonempty(p, "password", &password);
            let imap_host = crate::prompt::prompt_line("Email imap_host: ").await?;
            set_extra_string_if_nonempty(p, "imap_host", &imap_host);
            let smtp_host = crate::prompt::prompt_line("Email smtp_host: ").await?;
            set_extra_string_if_nonempty(p, "smtp_host", &smtp_host);
            let imap_port = crate::prompt::prompt_line("Email imap_port (default 993): ").await?;
            if let Ok(v) = imap_port.trim().parse::<u16>() {
                p.extra
                    .insert("imap_port".to_string(), serde_json::Value::from(v));
            }
            let smtp_port = crate::prompt::prompt_line("Email smtp_port (default 587): ").await?;
            if let Ok(v) = smtp_port.trim().parse::<u16>() {
                p.extra
                    .insert("smtp_port".to_string(), serde_json::Value::from(v));
            }
        }
        "sms" => {
            let sid = crate::prompt::prompt_line("Twilio account_sid: ").await?;
            set_extra_string_if_nonempty(p, "account_sid", &sid);
            let auth = crate::prompt::prompt_line("Twilio auth_token: ").await?;
            set_extra_string_if_nonempty(p, "auth_token", &auth);
            let from = crate::prompt::prompt_line("Twilio from_number (E.164): ").await?;
            set_extra_string_if_nonempty(p, "from_number", &from);
        }
        "homeassistant" => {
            let base_url =
                crate::prompt::prompt_line("HomeAssistant base_url (e.g. http://127.0.0.1:8123): ")
                    .await?;
            set_extra_string_if_nonempty(p, "base_url", &base_url);
            let token = crate::prompt::prompt_line("HomeAssistant long_lived_token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let webhook_id =
                crate::prompt::prompt_line("HomeAssistant webhook_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "webhook_id", &webhook_id);
        }
        "webhook" => {
            let secret = crate::prompt::prompt_line("Webhook secret: ").await?;
            set_extra_string_if_nonempty(p, "secret", &secret);
            let port = crate::prompt::prompt_line("Webhook port (default 9000): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let path = crate::prompt::prompt_line("Webhook path (default /webhook): ").await?;
            set_extra_string_if_nonempty(p, "path", &path);
        }
        "api_server" => {
            let host = crate::prompt::prompt_line("API server host (default 127.0.0.1): ").await?;
            set_extra_string_if_nonempty(p, "host", &host);
            let port = crate::prompt::prompt_line("API server port (default 8090): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let token = crate::prompt::prompt_line(
                "API server auth_token (required for non-loopback host): ",
            )
            .await?;
            set_extra_string_if_nonempty(p, "auth_token", &token);
        }
        _ => {}
    }
    Ok(())
}

pub(crate) async fn configure_gateway_platform(
    cli: &Cli,
    disk: &mut GatewayConfig,
    cfg_path: &std::path::Path,
    key: &str,
) -> Result<(), AgentError> {
    match key {
        "weixin" => {
            crate::auth_main::run_auth(
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
            *disk =
                load_user_config_file(cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
            let wx = disk
                .platforms
                .entry("weixin".to_string())
                .or_insert_with(PlatformConfig::default);
            wx.enabled = true;
            println!("Direct message policy: 1)pairing 2)open 3)allowlist 4)disabled");
            let dm_choice = crate::prompt::prompt_line("Choose [1-4] (default 1): ").await?;
            match dm_choice.trim() {
                "2" => {
                    wx.extra
                        .insert("dm_policy".to_string(), serde_json::json!("open"));
                    wx.extra
                        .insert("allow_from".to_string(), serde_json::json!([]));
                }
                "3" => {
                    let ids = parse_csv_list(
                        &crate::prompt::prompt_line("Allowed Weixin user IDs (comma-separated): ")
                            .await?,
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
            let group_choice = crate::prompt::prompt_line("Choose [1-3] (default 1): ").await?;
            match group_choice.trim() {
                "2" => {
                    wx.extra
                        .insert("group_policy".to_string(), serde_json::json!("open"));
                    wx.extra
                        .insert("group_allow_from".to_string(), serde_json::json!([]));
                }
                "3" => {
                    let ids = parse_csv_list(
                        &crate::prompt::prompt_line("Allowed Weixin group IDs (comma-separated): ")
                            .await?,
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
            let home = crate::prompt::prompt_line("Weixin home channel (optional): ").await?;
            if !home.trim().is_empty() {
                wx.home_channel = Some(home.trim().to_string());
            }
        }
        "wecom" => {
            crate::auth_main::run_auth(
                cli.clone(),
                Some("login".to_string()),
                Some("wecom".to_string()),
                None,
                None,
                None,
                None,
                true,
            )
            .await?;
            *disk =
                load_user_config_file(cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
        }
        "telegram" => {
            let token = resolve_telegram_bot_token_for_gateway_setup(disk).await?;
            let tg = disk
                .platforms
                .entry("telegram".to_string())
                .or_insert_with(PlatformConfig::default);
            tg.token = Some(token);
            tg.enabled = true;
            println!("Telegram bot token saved (platform enabled; written on Done).");

            println!(
                "Telegram user ID: message @userinfobot on Telegram to get your numeric user ID."
            );
            let allowed = crate::prompt::prompt_line(
                "Telegram allowed user IDs (comma-separated, required; use * for any user): ",
            )
            .await?;
            let allowed_users = parse_csv_list(&allowed);
            if allowed_users.is_empty() {
                return Err(AgentError::Config(
                    "Telegram setup requires at least one allowed user ID (or *)".into(),
                ));
            }
            apply_telegram_allowlists(tg, &allowed_users);

            let group_allowed = crate::prompt::prompt_line(
                "Telegram group-only allowed user IDs (comma-separated, optional): ",
            )
            .await?;
            let group_users = parse_csv_list(&group_allowed);
            if !group_users.is_empty() {
                tg.extra.insert(
                    "group_allow_from".to_string(),
                    serde_json::Value::Array(
                        group_users
                            .iter()
                            .cloned()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                );
            }

            let group_chats = crate::prompt::prompt_line(
                "Telegram allowed group chat IDs (comma-separated, optional): ",
            )
            .await?;
            let group_chat_ids = parse_csv_list(&group_chats);
            if !group_chat_ids.is_empty() {
                tg.extra.insert(
                    "group_allowed_chats".to_string(),
                    serde_json::Value::Array(
                        group_chat_ids
                            .iter()
                            .cloned()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                );
            }

            let polling = prompt_yes_no("Telegram use polling mode?", true).await?;
            tg.extra
                .insert("polling".to_string(), serde_json::Value::Bool(polling));
            if !polling {
                println!(
                    "Webhook mode: Telegram pushes updates to your HTTPS endpoint (HTTP API)."
                );
                let webhook_url = crate::prompt::prompt_line(
                    "Telegram webhook URL (HTTPS, e.g. https://my-app.example.com/telegram): ",
                )
                .await?;
                let webhook_url = webhook_url.trim();
                if webhook_url.is_empty() {
                    return Err(AgentError::Config(
                        "Telegram webhook URL is required when polling is disabled".into(),
                    ));
                }
                tg.webhook_url = Some(webhook_url.to_string());

                let mut webhook_secret = String::new();
                while webhook_secret.trim().is_empty() {
                    webhook_secret = crate::prompt::prompt_line(
                        "Telegram webhook secret (required; generate with: openssl rand -hex 32): ",
                    )
                    .await?;
                    if webhook_secret.trim().is_empty() {
                        println!("Webhook secret is required for Telegram HTTP API mode.");
                    }
                }
                tg.extra.insert(
                    "webhook_secret".to_string(),
                    serde_json::json!(webhook_secret.trim()),
                );

                let port =
                    crate::prompt::prompt_line("Telegram webhook listen port (default 8443): ")
                        .await?;
                if let Ok(v) = port.trim().parse::<u16>() {
                    tg.extra
                        .insert("webhook_port".to_string(), serde_json::Value::from(v));
                }
            }
            let home = crate::prompt::prompt_line("Telegram home channel (optional): ").await?;
            if !home.trim().is_empty() {
                tg.home_channel = Some(home.trim().to_string());
            }
        }
        "whatsapp" => {
            whatsapp_wizard::configure_whatsapp_for_gateway(disk).await?;
        }
        other => configure_platform_basic_prompts(disk, other).await?,
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Adapter registration
// ---------------------------------------------------------------------------

pub(crate) async fn register_gateway_adapters(
    config: &GatewayConfig,
    gateway: Arc<Gateway>,
    sidecar_tasks: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<(), AgentError> {
    if let Some(platform_cfg) = config.platforms.get("telegram") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let telegram_config = build_telegram_config(platform_cfg, token);
                let telegram_adapter = Arc::new(TelegramAdapter::new(telegram_config)?);
                telegram_adapter.start().await?;
                gateway
                    .register_adapter("telegram", telegram_adapter.clone())
                    .await;
                let gw_clone = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    crate::gateway_runtime::run_telegram_poll_loop(gw_clone, telegram_adapter)
                        .await;
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
                            crate::gateway_runtime::run_gateway_incoming_loop(
                                gw_clone, rx, "weixin",
                            )
                            .await;
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
                let discord_cfg = DiscordConfig::from_platform(platform_cfg, token);
                match DiscordAdapter::new(discord_cfg) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                        adapter.set_inbound_sender(tx).await;
                        gateway.register_adapter("discord", adapter.clone()).await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            crate::gateway_runtime::run_gateway_incoming_loop(
                                gw_clone, rx, "discord",
                            )
                            .await;
                        }));
                    }
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
            let wa_cfg = WhatsAppConfig::from_platform_config(platform_cfg);
            match WhatsAppAdapter::new(wa_cfg) {
                Ok(adapter) => {
                    let adapter = Arc::new(adapter);
                    let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                    adapter.set_inbound_sender(tx).await;
                    gateway.register_adapter("whatsapp", adapter).await;
                    let gw_clone = gateway.clone();
                    sidecar_tasks.push(tokio::spawn(async move {
                        crate::gateway_runtime::run_gateway_incoming_loop(gw_clone, rx, "whatsapp")
                            .await;
                    }));
                }
                Err(e) => println!("WhatsApp enabled but failed to initialize: {}", e),
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
                        crate::gateway_runtime::run_gateway_incoming_loop(gw_clone, rx, "dingtalk")
                            .await;
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
                    domain: extra_string(platform_cfg, "domain"),
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
            let wecom_cfg = WeComConfig::from_platform_config(platform_cfg);
            if wecom_cfg.bot_id.is_empty() || wecom_cfg.secret.is_empty() {
                println!(
                    "WeCom is enabled but bot_id/secret is missing (set platforms.wecom.extra.bot_id + secret or WECOM_BOT_ID/WECOM_SECRET); skipping wecom adapter."
                );
            } else {
                match WeComAdapter::new(wecom_cfg) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                        adapter.set_inbound_sender(tx).await;
                        gateway.register_adapter("wecom", adapter).await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            crate::gateway_runtime::run_gateway_incoming_loop(
                                gw_clone, rx, "wecom",
                            )
                            .await;
                        }));
                    }
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
                                spawn_gateway_route(gw_clone.clone(), incoming, "wecom_callback");
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
                        crate::gateway_runtime::run_gateway_incoming_loop(gw_clone, rx, "ntfy")
                            .await;
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
                    crate::gateway_runtime::run_webhook_inbound_loop(gw_clone, rx).await;
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
                crate::gateway_runtime::run_api_server_inbound_loop(gw_clone, rx).await;
            }));
            println!(
                "API server adapter enabled on {}:{}",
                api_cfg.host, api_cfg.port
            );
        }
    }

    Ok(())
}

/// Route one inbound message. Spawned per message so clarify replies can hit the
/// fast-path while another route for the same chat is blocked in `wait_for`.
pub(crate) fn spawn_gateway_route(
    gateway: Arc<Gateway>,
    incoming: GatewayIncomingMessage,
    platform: &str,
) {
    let platform = platform.to_string();
    tokio::spawn(async move {
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!(
                platform = %platform,
                error = %err,
                "Failed to route inbound gateway message"
            );
            let err_text = format!("⚠️ 请求处理失败，请稍后重试。({})", err);
            let _ = gateway
                .send_message(&incoming.platform, &incoming.chat_id, &err_text, None)
                .await;
        }
    });
}
