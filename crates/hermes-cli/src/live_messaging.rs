use std::sync::Arc;

use hermes_config::{extra_string, platform_token_or_extra, GatewayConfig, PlatformConfig};
use hermes_core::ToolHandler;
use hermes_gateway::gateway::GatewayConfig as RuntimeGatewayConfig;
use hermes_gateway::platforms::api_server::{ApiServerAdapter, ApiServerConfig};
use hermes_gateway::platforms::bluebubbles::{BlueBubblesAdapter, BlueBubblesConfig};
use hermes_gateway::platforms::dingtalk::{DingTalkAdapter, DingTalkConfig};
use hermes_gateway::platforms::discord::{DiscordAdapter, DiscordConfig};
use hermes_gateway::platforms::email::{EmailAdapter, EmailConfig};
use hermes_gateway::platforms::feishu::{FeishuAdapter, FeishuConfig};
use hermes_gateway::platforms::homeassistant::{HomeAssistantAdapter, HomeAssistantConfig};
use hermes_gateway::platforms::matrix::{MatrixAdapter, MatrixConfig};
use hermes_gateway::platforms::mattermost::{MattermostAdapter, MattermostConfig};
use hermes_gateway::platforms::qqbot::{QqBotAdapter, QqBotConfig};
use hermes_gateway::platforms::signal::{SignalAdapter, SignalConfig};
use hermes_gateway::platforms::slack::{SlackAdapter, SlackConfig};
use hermes_gateway::platforms::sms::{SmsAdapter, SmsConfig};
use hermes_gateway::platforms::telegram::{TelegramAdapter, TelegramConfig};
use hermes_gateway::platforms::webhook::{WebhookAdapter, WebhookConfig};
use hermes_gateway::platforms::wecom::{WeComAdapter, WeComConfig};
use hermes_gateway::platforms::weixin::{WeChatAdapter, WeixinConfig};
use hermes_gateway::platforms::whatsapp::{WhatsAppAdapter, WhatsAppConfig};
use hermes_gateway::{
    evaluate_gateway_requirements, DmManager, Gateway, PlatformAdapter, RequirementScope,
    RequirementSeverity,
    SessionManager,
};
use hermes_tools::tools::messaging::SendMessageHandler;
use hermes_tools::ToolRegistry;

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

fn register_live_send_message_tool(registry: &ToolRegistry, gateway: Arc<Gateway>) {
    let handler = SendMessageHandler::new(Arc::new(
        hermes_gateway::tool_backends::GatewayMessagingBackend::new(gateway),
    ));
    let schema = handler.schema();
    let name = schema.name.clone();
    let desc = schema.description.clone();
    registry.register(
        name,
        "messaging",
        schema,
        Arc::new(handler),
        Arc::new(|| true),
        vec![],
        true,
        desc,
        "💬",
        None,
    );
}

async fn register_outbound_adapters(config: &GatewayConfig, gateway: &Arc<Gateway>) -> Vec<String> {
    use std::collections::HashSet;

    let fatal_platforms: HashSet<String> =
        evaluate_gateway_requirements(config, RequirementScope::LiveMessaging)
            .into_iter()
            .filter(|i| i.severity == RequirementSeverity::Fatal)
            .map(|i| i.platform)
            .collect();

    let mut registered = Vec::new();

    if let Some(platform_cfg) = config.platforms.get("telegram") {
        if platform_cfg.enabled && !fatal_platforms.contains("telegram") {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
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
                let telegram_cfg = TelegramConfig {
                    token,
                    webhook_url: platform_cfg.webhook_url.clone(),
                    polling,
                    proxy: Default::default(),
                    parse_markdown,
                    parse_html,
                    poll_timeout,
                    bot_username: None,
                };
                match TelegramAdapter::new(telegram_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("telegram", Arc::new(adapter))
                            .await;
                        registered.push("telegram".to_string());
                    }
                    Err(e) => tracing::warn!("telegram adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("discord") {
        if platform_cfg.enabled && !fatal_platforms.contains("discord") {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let discord_cfg = DiscordConfig {
                    token,
                    application_id: extra_string(platform_cfg, "application_id"),
                    proxy: Default::default(),
                    require_mention: platform_cfg.require_mention.unwrap_or(true),
                    intents: platform_cfg
                        .extra
                        .get("intents")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(hermes_gateway::platforms::discord::default_intents()),
                };
                match DiscordAdapter::new(discord_cfg) {
                    Ok(adapter) => {
                        if adapter.start().await.is_ok() {
                            gateway.register_adapter("discord", Arc::new(adapter)).await;
                            registered.push("discord".to_string());
                        } else {
                            tracing::warn!("discord adapter start failed");
                        }
                    }
                    Err(e) => tracing::warn!("discord adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("slack") {
        if platform_cfg.enabled && !fatal_platforms.contains("slack") {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let slack_cfg = SlackConfig {
                    token,
                    app_token: extra_string(platform_cfg, "app_token"),
                    socket_mode: extra_bool(platform_cfg, "socket_mode", false),
                    reactions: extra_bool(platform_cfg, "reactions", true),
                    proxy: Default::default(),
                };
                match SlackAdapter::new(slack_cfg) {
                    Ok(adapter) => {
                        gateway.register_adapter("slack", Arc::new(adapter)).await;
                        registered.push("slack".to_string());
                    }
                    Err(e) => tracing::warn!("slack adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("matrix") {
        if platform_cfg.enabled && !fatal_platforms.contains("matrix") {
            let homeserver_url = extra_string(platform_cfg, "homeserver_url")
                .or_else(|| extra_string(platform_cfg, "homeserver"))
                .unwrap_or_default();
            let user_id = extra_string(platform_cfg, "user_id").unwrap_or_default();
            let access_token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "access_token"))
                .unwrap_or_default();
            if !homeserver_url.is_empty() && !user_id.is_empty() && !access_token.is_empty() {
                let matrix_cfg = MatrixConfig {
                    homeserver_url,
                    user_id,
                    access_token,
                    room_id: extra_string(platform_cfg, "room_id"),
                    proxy: Default::default(),
                };
                match MatrixAdapter::new(matrix_cfg) {
                    Ok(adapter) => {
                        gateway.register_adapter("matrix", Arc::new(adapter)).await;
                        registered.push("matrix".to_string());
                    }
                    Err(e) => tracing::warn!("matrix adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("mattermost") {
        if platform_cfg.enabled && !fatal_platforms.contains("mattermost") {
            let token = platform_token_or_extra(platform_cfg).unwrap_or_default();
            let server_url = extra_string(platform_cfg, "server_url")
                .or_else(|| extra_string(platform_cfg, "url"))
                .unwrap_or_default();
            if !token.is_empty() && !server_url.is_empty() {
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
                            .await;
                        registered.push("mattermost".to_string());
                    }
                    Err(e) => tracing::warn!("mattermost adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("signal") {
        if platform_cfg.enabled && !fatal_platforms.contains("signal") {
            let phone_number = extra_string(platform_cfg, "phone_number")
                .or_else(|| extra_string(platform_cfg, "account"))
                .unwrap_or_default();
            if !phone_number.is_empty() {
                let signal_cfg = SignalConfig {
                    phone_number,
                    api_url: extra_string(platform_cfg, "api_url")
                        .unwrap_or_else(|| "http://localhost:8080".to_string()),
                    proxy: Default::default(),
                };
                match SignalAdapter::new(signal_cfg) {
                    Ok(adapter) => {
                        gateway.register_adapter("signal", Arc::new(adapter)).await;
                        registered.push("signal".to_string());
                    }
                    Err(e) => tracing::warn!("signal adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("whatsapp") {
        if platform_cfg.enabled && !fatal_platforms.contains("whatsapp") {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let wa_cfg = WhatsAppConfig {
                    token,
                    phone_number_id: extra_string(platform_cfg, "phone_number_id"),
                    business_account_id: extra_string(platform_cfg, "business_account_id"),
                    verify_token: extra_string(platform_cfg, "verify_token"),
                    proxy: Default::default(),
                };
                match WhatsAppAdapter::new(wa_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("whatsapp", Arc::new(adapter))
                            .await;
                        registered.push("whatsapp".to_string());
                    }
                    Err(e) => tracing::warn!("whatsapp adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("dingtalk") {
        if platform_cfg.enabled && !fatal_platforms.contains("dingtalk") {
            let ding_cfg = DingTalkConfig::from_platform_config(platform_cfg);
            match DingTalkAdapter::new(ding_cfg) {
                Ok(adapter) => {
                    gateway
                        .register_adapter("dingtalk", Arc::new(adapter))
                        .await;
                    registered.push("dingtalk".to_string());
                }
                Err(e) => tracing::warn!("dingtalk adapter init failed: {}", e),
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("feishu") {
        if platform_cfg.enabled && !fatal_platforms.contains("feishu") {
            let app_id = extra_string(platform_cfg, "app_id").unwrap_or_default();
            let app_secret = extra_string(platform_cfg, "app_secret").unwrap_or_default();
            if !app_id.is_empty() && !app_secret.is_empty() {
                let feishu_cfg = FeishuConfig {
                    app_id,
                    app_secret,
                    verification_token: extra_string(platform_cfg, "verification_token"),
                    encrypt_key: extra_string(platform_cfg, "encrypt_key"),
                    proxy: Default::default(),
                };
                match FeishuAdapter::new(feishu_cfg) {
                    Ok(adapter) => {
                        gateway.register_adapter("feishu", Arc::new(adapter)).await;
                        registered.push("feishu".to_string());
                    }
                    Err(e) => tracing::warn!("feishu adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("wecom") {
        if platform_cfg.enabled && !fatal_platforms.contains("wecom") {
            let bot_id = extra_string(platform_cfg, "bot_id")
                .or_else(|| extra_string(platform_cfg, "corp_id"))
                .unwrap_or_default();
            let secret = extra_string(platform_cfg, "secret").unwrap_or_default();
            if !bot_id.is_empty() && !secret.is_empty() {
                let wecom_cfg = WeComConfig {
                    bot_id,
                    secret,
                    websocket_url: extra_string(platform_cfg, "websocket_url")
                        .unwrap_or_else(|| "wss://openws.work.weixin.qq.com".to_string()),
                    dm_policy: extra_string(platform_cfg, "dm_policy")
                        .unwrap_or_else(|| "open".to_string()),
                    allow_from: platform_cfg
                        .extra
                        .get("allow_from")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    group_policy: extra_string(platform_cfg, "group_policy")
                        .unwrap_or_else(|| "open".to_string()),
                    group_allow_from: platform_cfg
                        .extra
                        .get("group_allow_from")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    groups: platform_cfg
                        .extra
                        .get("groups")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    proxy: Default::default(),
                };
                match WeComAdapter::new(wecom_cfg) {
                    Ok(adapter) => {
                        gateway.register_adapter("wecom", Arc::new(adapter)).await;
                        registered.push("wecom".to_string());
                    }
                    Err(e) => tracing::warn!("wecom adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config
        .platforms
        .get("qqbot")
        .or_else(|| config.platforms.get("qq"))
    {
        if platform_cfg.enabled && !fatal_platforms.contains("qqbot") {
            let app_id = extra_string(platform_cfg, "app_id").unwrap_or_default();
            let client_secret = extra_string(platform_cfg, "client_secret").unwrap_or_default();
            if !app_id.is_empty() && !client_secret.is_empty() {
                let qq_cfg = QqBotConfig {
                    app_id,
                    client_secret,
                    markdown_support: extra_bool(platform_cfg, "markdown_support", true),
                    proxy: Default::default(),
                };
                match QqBotAdapter::new(qq_cfg) {
                    Ok(adapter) => {
                        gateway.register_adapter("qqbot", Arc::new(adapter)).await;
                        registered.push("qqbot".to_string());
                    }
                    Err(e) => tracing::warn!("qqbot adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("bluebubbles") {
        if platform_cfg.enabled && !fatal_platforms.contains("bluebubbles") {
            let server_url = extra_string(platform_cfg, "server_url").unwrap_or_default();
            let password = extra_string(platform_cfg, "password").unwrap_or_default();
            if !server_url.is_empty() && !password.is_empty() {
                let bb_cfg = BlueBubblesConfig {
                    server_url,
                    password,
                    proxy: Default::default(),
                };
                match BlueBubblesAdapter::new(bb_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("bluebubbles", Arc::new(adapter))
                            .await;
                        registered.push("bluebubbles".to_string());
                    }
                    Err(e) => tracing::warn!("bluebubbles adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("email") {
        if platform_cfg.enabled && !fatal_platforms.contains("email") {
            let imap_host = extra_string(platform_cfg, "imap_host").unwrap_or_default();
            let smtp_host = extra_string(platform_cfg, "smtp_host").unwrap_or_default();
            let username = extra_string(platform_cfg, "username").unwrap_or_default();
            let password = extra_string(platform_cfg, "password").unwrap_or_default();
            if !imap_host.is_empty()
                && !smtp_host.is_empty()
                && !username.is_empty()
                && !password.is_empty()
            {
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
                    Ok(adapter) => {
                        gateway.register_adapter("email", Arc::new(adapter)).await;
                        registered.push("email".to_string());
                    }
                    Err(e) => tracing::warn!("email adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("sms") {
        if platform_cfg.enabled && !fatal_platforms.contains("sms") {
            let account_sid = extra_string(platform_cfg, "account_sid").unwrap_or_default();
            let auth_token = extra_string(platform_cfg, "auth_token").unwrap_or_default();
            let from_number = extra_string(platform_cfg, "from_number").unwrap_or_default();
            if !account_sid.is_empty() && !auth_token.is_empty() && !from_number.is_empty() {
                let sms_cfg = SmsConfig {
                    provider: extra_string(platform_cfg, "provider")
                        .unwrap_or_else(|| "twilio".to_string()),
                    account_sid,
                    auth_token,
                    from_number,
                    proxy: Default::default(),
                };
                match SmsAdapter::new(sms_cfg) {
                    Ok(adapter) => {
                        gateway.register_adapter("sms", Arc::new(adapter)).await;
                        registered.push("sms".to_string());
                    }
                    Err(e) => tracing::warn!("sms adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("homeassistant") {
        if platform_cfg.enabled && !fatal_platforms.contains("homeassistant") {
            let base_url = extra_string(platform_cfg, "base_url").unwrap_or_default();
            let long_lived_token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "long_lived_token"))
                .unwrap_or_default();
            if !base_url.is_empty() && !long_lived_token.is_empty() {
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
                            .await;
                        registered.push("homeassistant".to_string());
                    }
                    Err(e) => tracing::warn!("homeassistant adapter init failed: {}", e),
                }
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("webhook") {
        if platform_cfg.enabled && !fatal_platforms.contains("webhook") {
            let secret = extra_string(platform_cfg, "secret").unwrap_or_default();
            if !secret.is_empty() {
                let wh_cfg = WebhookConfig {
                    port: extra_u16(platform_cfg, "port", 9000),
                    path: extra_string(platform_cfg, "path")
                        .unwrap_or_else(|| "/webhook".to_string()),
                    secret,
                };
                let adapter = WebhookAdapter::new(wh_cfg);
                gateway.register_adapter("webhook", Arc::new(adapter)).await;
                registered.push("webhook".to_string());
            }
        }
    }

    if let Some(platform_cfg) = config.platforms.get("api_server") {
        if platform_cfg.enabled {
            let api_cfg = ApiServerConfig {
                host: extra_string(platform_cfg, "host").unwrap_or_else(|| "0.0.0.0".to_string()),
                port: extra_u16(platform_cfg, "port", 8090),
                auth_token: extra_string(platform_cfg, "auth_token"),
            };
            let adapter = ApiServerAdapter::new(api_cfg);
            gateway
                .register_adapter("api_server", Arc::new(adapter))
                .await;
            registered.push("api_server".to_string());
        }
    }

    if let Some(platform_cfg) = config.platforms.get("weixin") {
        if platform_cfg.enabled && !fatal_platforms.contains("weixin") {
            let cfg = WeixinConfig::from_platform_config(platform_cfg);
            match WeChatAdapter::new(cfg) {
                Ok(adapter) => {
                    let adapter = Arc::new(adapter);
                    let (inbound_tx, mut inbound_rx) =
                        tokio::sync::mpsc::channel::<hermes_gateway::gateway::IncomingMessage>(256);
                    adapter.set_inbound_sender(inbound_tx).await;
                    let gw_clone = gateway.clone();
                    tokio::spawn(async move {
                        while let Some(msg) = inbound_rx.recv().await {
                            if let Err(e) = gw_clone.route_message(&msg).await {
                                tracing::warn!(
                                    platform = %msg.platform,
                                    chat_id = %msg.chat_id,
                                    error = %e,
                                    "weixin inbound routing failed"
                                );
                            }
                        }
                    });
                    gateway.register_adapter("weixin", adapter).await;
                    registered.push("weixin".to_string());
                }
                Err(e) => tracing::warn!("weixin adapter init failed: {}", e),
            }
        }
    }

    registered
}

pub async fn enable_live_messaging_tool(
    config: &GatewayConfig,
    tool_registry: &ToolRegistry,
) -> usize {
    let runtime_gateway_config = RuntimeGatewayConfig {
        streaming_enabled: false,
        ..RuntimeGatewayConfig::default()
    };
    let session_manager = Arc::new(SessionManager::new(config.session.clone()));
    let gateway = Arc::new(Gateway::new(
        session_manager,
        DmManager::with_ignore_behavior(),
        runtime_gateway_config,
    ));

    let registered = register_outbound_adapters(config, &gateway).await;
    if registered.is_empty() {
        return 0;
    }

    register_live_send_message_tool(tool_registry, gateway);
    registered.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hermes_core::{GatewayError, ParseMode, PlatformAdapter};
    use hermes_tools::backends::messaging::SignalMessagingBackend;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingAdapter {
        sends: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PlatformAdapter for RecordingAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &str,
            _text: &str,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_file(
            &self,
            _chat_id: &str,
            _file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            "telegram"
        }
    }

    #[tokio::test]
    async fn live_send_message_override_dispatches_to_gateway() {
        let registry = ToolRegistry::new();

        let default_handler = SendMessageHandler::new(Arc::new(SignalMessagingBackend::new()));
        let default_schema = default_handler.schema();
        let default_name = default_schema.name.clone();
        let default_desc = default_schema.description.clone();
        registry.register(
            default_name,
            "messaging",
            default_schema,
            Arc::new(default_handler),
            Arc::new(|| true),
            vec![],
            true,
            default_desc,
            "💬",
            None,
        );

        let session_manager = Arc::new(SessionManager::new(Default::default()));
        let gateway = Arc::new(Gateway::new(
            session_manager,
            DmManager::with_ignore_behavior(),
            RuntimeGatewayConfig::default(),
        ));
        let sends = Arc::new(AtomicUsize::new(0));
        gateway
            .register_adapter(
                "telegram",
                Arc::new(RecordingAdapter {
                    sends: sends.clone(),
                }),
            )
            .await;

        register_live_send_message_tool(&registry, gateway);
        let out = registry
            .dispatch_async(
                "send_message",
                json!({
                    "platform":"telegram",
                    "recipient":"123",
                    "message":"hello"
                }),
            )
            .await;

        assert!(
            out.contains("delivered"),
            "expected real delivery status, got: {out}"
        );
        assert_eq!(sends.load(Ordering::SeqCst), 1);
    }
}
