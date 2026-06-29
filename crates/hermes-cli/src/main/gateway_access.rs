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

#[derive(Clone)]
struct GatewayAgentBusyControl {
    interrupt: hermes_agent::InterruptController,
}

impl GatewayAgentBusyControl {
    fn new(interrupt: hermes_agent::InterruptController) -> Self {
        Self { interrupt }
    }
}

impl ActiveSessionControl for GatewayAgentBusyControl {
    fn interrupt(&self, message: &str) {
        self.interrupt.interrupt(Some(message.to_string()));
    }

    fn steer(&self, message: &str) -> bool {
        self.interrupt
            .interrupt(Some(hermes_agent::format_steer_marker(message)));
        true
    }
}

fn normalize_gateway_tool_progress_mode(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "none" | "false" | "0" => Some("off".to_string()),
        "new" => Some("new".to_string()),
        "all" | "true" | "1" => Some("all".to_string()),
        "verbose" => Some("verbose".to_string()),
        _ => None,
    }
}

fn resolve_gateway_tool_progress_mode(
    config: &hermes_config::GatewayConfig,
    platform: &str,
    session_override: Option<&str>,
) -> String {
    if let Some(mode) = session_override.and_then(normalize_gateway_tool_progress_mode) {
        return mode;
    }

    let platform_key = platform.trim().to_ascii_lowercase().replace('-', "_");
    config
        .display
        .platform_tool_progress(&platform_key)
        .and_then(normalize_gateway_tool_progress_mode)
        .unwrap_or_else(|| match platform_key.as_str() {
            "telegram" | "slack" => "off".to_string(),
            _ => "all".to_string(),
        })
}

fn should_emit_gateway_tool_progress(
    mode: &str,
    tool_name: &str,
    seen_tools: &Arc<Mutex<HashSet<String>>>,
) -> bool {
    match mode {
        "off" => false,
        "new" => seen_tools
            .lock()
            .map(|mut seen| seen.insert(tool_name.to_string()))
            .unwrap_or(true),
        "all" | "verbose" => true,
        _ => false,
    }
}

fn gateway_tool_progress_parse_mode(platform: &str, text: &str) -> Option<ParseMode> {
    let markdown_code_block = text.trim_start().starts_with("```") || text.contains("\n```");
    if markdown_code_block && platform.eq_ignore_ascii_case("telegram") {
        Some(ParseMode::Markdown)
    } else {
        None
    }
}

fn build_agent_for_gateway_context(
    config: &hermes_config::GatewayConfig,
    ctx: &GatewayRuntimeContext,
    agent_tools: Arc<hermes_agent::agent_loop::ToolRegistry>,
) -> AgentLoop {
    let effective_model =
        resolve_model_for_gateway(config.model.as_deref().unwrap_or("gpt-5.5"), ctx);
    let effective_model = select_startup_model_with_fallback_and_auth_resolver(
        config,
        &effective_model,
        Some(&provider_oauth_token_from_auth_state),
    )
    .selected_model;
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

#[cfg(feature = "gateway-telegram")]
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
        disable_link_previews: extra_bool(platform_cfg, "disable_link_previews", false),
        rich_messages: extra_bool(platform_cfg, "rich_messages", false),
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
        allowed_users: telegram_allowed_users_for_adapter(platform_cfg),
        group_allowed_users: telegram_group_allowed_users_for_adapter(platform_cfg),
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
        command_menu_enabled: telegram_command_menu_bool(platform_cfg, "enabled", true),
        command_menu_max_commands: telegram_command_menu_usize(platform_cfg, "max_commands", 60),
        command_menu_priority: telegram_command_menu_string_vec(platform_cfg, "priority"),
        command_menu_priority_mode: telegram_command_menu_string(
            platform_cfg,
            "priority_mode",
            "prepend",
        ),
    }
}

#[cfg(feature = "gateway-telegram")]
fn telegram_allowed_users_for_adapter(
    platform_cfg: &hermes_config::platform::PlatformConfig,
) -> Vec<String> {
    let mut users = platform_cfg.allowed_users.clone();
    users.extend(platform_cfg.admin_users.clone());
    for key in GATEWAY_CONFIG_DIRECT_USER_ALLOWLIST_EXTRA_KEYS {
        users.extend(extra_string_vec(platform_cfg, key));
    }
    if gateway_platform_config_allows_all_users(platform_cfg) {
        users.push("*".to_string());
    }
    users
}

#[cfg(feature = "gateway-telegram")]
fn telegram_group_allowed_users_for_adapter(
    platform_cfg: &hermes_config::platform::PlatformConfig,
) -> Vec<String> {
    let mut users = Vec::new();
    for key in GATEWAY_CONFIG_GROUP_USER_ALLOWLIST_EXTRA_KEYS {
        users.extend(extra_string_vec(platform_cfg, key));
    }
    users
}

#[cfg(feature = "gateway-telegram")]
fn telegram_command_menu_value<'a>(
    platform_cfg: &'a hermes_config::platform::PlatformConfig,
    key: &str,
) -> Option<&'a serde_json::Value> {
    platform_cfg
        .extra
        .get("command_menu")
        .and_then(serde_json::Value::as_object)
        .and_then(|menu| menu.get(key))
        .or_else(|| platform_cfg.extra.get(&format!("command_menu_{key}")))
}

#[cfg(feature = "gateway-telegram")]
fn telegram_command_menu_bool(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
    default: bool,
) -> bool {
    telegram_command_menu_value(platform_cfg, key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default)
}

#[cfg(feature = "gateway-telegram")]
fn telegram_command_menu_usize(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
    default: usize,
) -> usize {
    telegram_command_menu_value(platform_cfg, key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

#[cfg(feature = "gateway-telegram")]
fn telegram_command_menu_string(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
    default: &str,
) -> String {
    telegram_command_menu_value(platform_cfg, key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

#[cfg(feature = "gateway-telegram")]
fn telegram_command_menu_string_vec(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
) -> Vec<String> {
    let Some(raw) = telegram_command_menu_value(platform_cfg, key) else {
        return Vec::new();
    };
    match raw {
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(|value| value.as_str().map(str::trim))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        serde_json::Value::String(value) => value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
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

#[cfg(any(
    feature = "gateway-slack",
    feature = "gateway-telegram",
    feature = "gateway-whatsapp"
))]
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

#[cfg(feature = "gateway-discord")]
fn discord_reply_to_mode_string(platform_cfg: &PlatformConfig) -> Option<String> {
    reply_to_mode_string(platform_cfg)
}

#[cfg(any(feature = "gateway-discord", feature = "gateway-telegram"))]
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

#[cfg(any(test, feature = "gateway-matrix"))]
fn matrix_home_room_for_platform(platform_cfg: &PlatformConfig) -> Option<String> {
    extra_string(platform_cfg, "room_id")
        .or_else(|| extra_string(platform_cfg, "home_room"))
        .or_else(|| env_string("MATRIX_HOME_ROOM"))
}

#[cfg(any(
    feature = "gateway-qqbot",
    feature = "gateway-slack",
    feature = "gateway-telegram",
    feature = "gateway-whatsapp"
))]
fn extra_bool(platform_cfg: &PlatformConfig, key: &str, default: bool) -> bool {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

#[cfg(any(
    feature = "gateway-api-server",
    feature = "gateway-email",
    feature = "gateway-webhook",
    feature = "gateway-wecom-callback"
))]
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

#[cfg(feature = "gateway-email")]
fn env_email_identity_vec(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .replace('\n', ",")
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
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

fn missing_gateway_adapter_feature(platform: &str) -> Option<&'static str> {
    match platform
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "telegram" if !cfg!(feature = "gateway-telegram") => Some("gateway-telegram"),
        "discord" if !cfg!(feature = "gateway-discord") => Some("gateway-discord"),
        "slack" if !cfg!(feature = "gateway-slack") => Some("gateway-slack"),
        "whatsapp" if !cfg!(feature = "gateway-whatsapp") => Some("gateway-whatsapp"),
        "signal" if !cfg!(feature = "gateway-signal") => Some("gateway-signal"),
        "matrix" if !cfg!(feature = "gateway-matrix") => Some("gateway-matrix"),
        "mattermost" if !cfg!(feature = "gateway-mattermost") => Some("gateway-mattermost"),
        "dingtalk" if !cfg!(feature = "gateway-dingtalk") => Some("gateway-dingtalk"),
        "feishu" if !cfg!(feature = "gateway-feishu") => Some("gateway-feishu"),
        "wecom" if !cfg!(feature = "gateway-wecom") => Some("gateway-wecom"),
        "wecom_callback" if !cfg!(feature = "gateway-wecom-callback") => {
            Some("gateway-wecom-callback")
        }
        "weixin" if !cfg!(feature = "gateway-weixin") => Some("gateway-weixin"),
        "qqbot" | "qq" if !cfg!(feature = "gateway-qqbot") => Some("gateway-qqbot"),
        "bluebubbles" if !cfg!(feature = "gateway-bluebubbles") => Some("gateway-bluebubbles"),
        "email" if !cfg!(feature = "gateway-email") => Some("gateway-email"),
        "sms" if !cfg!(feature = "gateway-sms") => Some("gateway-sms"),
        "homeassistant" if !cfg!(feature = "gateway-homeassistant") => {
            Some("gateway-homeassistant")
        }
        "ntfy" if !cfg!(feature = "gateway-ntfy") => Some("gateway-ntfy"),
        "api_server" if !cfg!(feature = "gateway-api-server") => Some("gateway-api-server"),
        "webhook" if !cfg!(feature = "gateway-webhook") => Some("gateway-webhook"),
        _ => None,
    }
}
