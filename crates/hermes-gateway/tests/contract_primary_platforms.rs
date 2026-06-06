use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use hermes_config::session::SessionConfig;
use hermes_core::{GatewayError, MessageRole};
use hermes_gateway::dm::DmManager;
use hermes_gateway::gateway::{GatewayConfig, IncomingMessage};
use hermes_gateway::{Gateway, ParseMode, PlatformAdapter, SessionManager};

#[derive(Default)]
struct AdapterState {
    running: AtomicBool,
    start_calls: AtomicUsize,
    stop_calls: AtomicUsize,
    sent_texts: Mutex<Vec<String>>,
}

struct ContractAdapter {
    name: &'static str,
    state: Arc<AdapterState>,
}

impl ContractAdapter {
    fn new(name: &'static str) -> (Arc<Self>, Arc<AdapterState>) {
        let state = Arc::new(AdapterState::default());
        (
            Arc::new(Self {
                name,
                state: state.clone(),
            }),
            state,
        )
    }
}

#[async_trait]
impl PlatformAdapter for ContractAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        self.state.start_calls.fetch_add(1, Ordering::SeqCst);
        self.state.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        self.state.stop_calls.fetch_add(1, Ordering::SeqCst);
        self.state.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.state.sent_texts.lock().unwrap().push(text.to_string());
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
        self.state.running.load(Ordering::SeqCst)
    }

    fn platform_name(&self) -> &str {
        self.name
    }
}

fn incoming(platform: &str, user_id: &str, text: &str) -> IncomingMessage {
    IncomingMessage {
        platform: platform.to_string(),
        chat_id: format!("chat-{platform}"),
        user_id: user_id.to_string(),
        text: text.to_string(),
        media_urls: vec![],
        media_types: vec![],
        message_id: None,
        is_dm: true,
        interaction_id: None,
        interaction_token: None,
    role_ids: vec![],
        ..Default::default()
    }
}

#[tokio::test]
async fn contract_startup_and_routing_for_primary_platforms() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());

    let mut states = Vec::new();
    for name in ["telegram", "weixin", "discord", "slack"] {
        let (adapter, state) = ContractAdapter::new(name);
        states.push(state);
        gateway.register_adapter(name, adapter).await;
    }

    gateway
        .set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let user_text = messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::User)
                    .and_then(|m| m.content.clone())
                    .unwrap_or_else(|| "<missing-user>".to_string());
                Ok(format!("ack:{user_text}"))
            })
        }))
        .await;

    gateway
        .start_all()
        .await
        .expect("all adapters should start");
    for state in &states {
        assert_eq!(state.start_calls.load(Ordering::SeqCst), 1);
        assert!(state.running.load(Ordering::SeqCst));
    }

    for name in ["telegram", "weixin", "discord", "slack"] {
        gateway
            .route_message(&incoming(name, "u1", &format!("hello-{name}")))
            .await
            .expect("message routing should succeed");
    }

    for (idx, state) in states.iter().enumerate() {
        let sent = state.sent_texts.lock().unwrap();
        assert!(
            sent.iter().any(|m| m.contains("ack:hello-")),
            "adapter index {idx} should receive assistant reply"
        );
    }
}

#[tokio::test]
async fn contract_auth_gate_denies_unauthorized_dm_on_primary_platforms() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm = DmManager::with_ignore_behavior();
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let handler_calls = Arc::new(AtomicUsize::new(0));

    let mut states = Vec::new();
    for name in ["telegram", "weixin", "discord", "slack"] {
        let (adapter, state) = ContractAdapter::new(name);
        states.push(state);
        gateway.register_adapter(name, adapter).await;
    }

    let handler_calls_clone = handler_calls.clone();
    gateway
        .set_message_handler(Arc::new(move |_| {
            let calls = handler_calls_clone.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok("should-not-run".to_string())
            })
        }))
        .await;

    for name in ["telegram", "weixin", "discord", "slack"] {
        gateway
            .route_message(&incoming(name, "unknown-user", "hello"))
            .await
            .expect("unauthorized DM should be ignored without error");
    }

    assert_eq!(
        handler_calls.load(Ordering::SeqCst),
        0,
        "handler should not be called for denied DMs"
    );
    for state in &states {
        assert!(state.sent_texts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn contract_duplicate_message_id_redelivery_is_suppressed() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;

    let handler_calls = Arc::new(AtomicUsize::new(0));
    let handler_calls_clone = handler_calls.clone();
    gateway
        .set_message_handler(Arc::new(move |messages| {
            let calls = handler_calls_clone.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let latest = messages
                    .last()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(format!("ack:{latest}"))
            })
        }))
        .await;

    let incoming = IncomingMessage {
        message_id: Some("platform-msg-42".to_string()),
        ..incoming("telegram", "u1", "redeliver me once")
    };

    gateway
        .route_message(&incoming)
        .await
        .expect("first delivery should route");
    gateway
        .route_message(&incoming)
        .await
        .expect("duplicate redelivery should be acknowledged but suppressed");

    assert_eq!(handler_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.sent_texts.lock().unwrap().len(), 1);
    assert_eq!(
        gateway
            .session_transcript_len("telegram", "chat-telegram", "u1")
            .await,
        2,
        "duplicate redelivery must not append another user/assistant exchange"
    );
}

#[tokio::test]
async fn contract_duplicate_message_id_scope_preserves_distinct_chats() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = calls.clone();
    gateway
        .set_message_handler(Arc::new(move |messages| {
            let calls = calls_for_handler.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(messages
                    .last()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default())
            })
        }))
        .await;

    let first = IncomingMessage {
        message_id: Some("same-platform-id".to_string()),
        ..incoming("telegram", "u1", "chat one")
    };
    let second_chat = IncomingMessage {
        chat_id: "chat-two".to_string(),
        message_id: Some("same-platform-id".to_string()),
        ..incoming("telegram", "u1", "chat two")
    };

    gateway.route_message(&first).await.expect("first chat");
    gateway
        .route_message(&second_chat)
        .await
        .expect("same platform id in a different chat must route");

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(state.sent_texts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn contract_streaming_final_response_is_delivered_before_deferred_messages() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let config = GatewayConfig {
        streaming_enabled: true,
        ..GatewayConfig::default()
    };
    let gateway = Gateway::new(session_manager, dm, config);
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;

    gateway
        .set_streaming_handler_with_context(Arc::new(|_messages, ctx, _on_chunk| {
            Box::pin(async move {
                ctx.deferred_post_delivery_messages
                    .expect("deferred queue")
                    .lock()
                    .unwrap()
                    .push("deferred-after-final".to_string());
                Ok("stream-final".to_string())
            })
        }))
        .await;

    gateway
        .route_message(&incoming("telegram", "u1", "stream this"))
        .await
        .expect("streaming route should succeed");

    assert_eq!(
        state.sent_texts.lock().unwrap().clone(),
        vec![
            "...".to_string(),
            "stream-final".to_string(),
            "deferred-after-final".to_string()
        ]
    );
}

#[tokio::test]
async fn contract_background_task_completion_notifies_origin_chat() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;
    gateway
        .set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let prompt = messages
                    .last()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(format!("background-result:{prompt}"))
            })
        }))
        .await;

    gateway
        .route_message(&IncomingMessage {
            text: "/background collect diagnostics".to_string(),
            ..incoming("telegram", "u1", "")
        })
        .await
        .expect("background command should be accepted");

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if state
                .sent_texts
                .lock()
                .unwrap()
                .iter()
                .any(|m| m.contains("Background task") && m.contains("background-result"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background completion notification should be delivered");
}

#[tokio::test]
async fn contract_background_task_failure_notifies_origin_chat() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;
    gateway
        .set_message_handler(Arc::new(|_messages| {
            Box::pin(async move { Err(GatewayError::Platform("tool crashed".to_string())) })
        }))
        .await;

    gateway
        .route_message(&IncomingMessage {
            text: "/btw inspect failure".to_string(),
            ..incoming("telegram", "u1", "")
        })
        .await
        .expect("btw command should be accepted");

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if state
                .sent_texts
                .lock()
                .unwrap()
                .iter()
                .any(|m| m.contains("/btw failed") && m.contains("tool crashed"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background failure notification should be delivered");
}

#[tokio::test]
async fn contract_runtime_commands_preserve_session_and_usage_state() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;
    gateway
        .set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let latest = messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::User)
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(format!("assistant:{latest}"))
            })
        }))
        .await;

    for text in ["first question", "second question"] {
        gateway
            .route_message(&incoming("telegram", "u1", text))
            .await
            .expect("normal turn");
    }
    assert_eq!(
        gateway
            .session_transcript_len("telegram", "chat-telegram", "u1")
            .await,
        4
    );

    for command in [
        "/model gpt-4o",
        "/provider openai",
        "/profile prod",
        "/branch feature/gateway-contracts",
        "/fast",
        "/budget 42",
        "/usage",
        "/status",
        "/title Runtime QA",
        "/resume",
        "/voice",
        "/update",
        "/retry",
        "/rollback 2",
    ] {
        gateway
            .route_message(&IncomingMessage {
                text: command.to_string(),
                ..incoming("telegram", "u1", "")
            })
            .await
            .unwrap_or_else(|err| panic!("{command} should route: {err}"));
    }

    let sent = state.sent_texts.lock().unwrap().clone();
    assert!(sent.iter().any(|m| m.contains("Model switched to: gpt-4o")));
    assert!(sent
        .iter()
        .any(|m| m.contains("Provider switched to: openai")));
    assert!(sent.iter().any(|m| m.contains("Profile switched to: prod")));
    assert!(sent
        .iter()
        .any(|m| m.contains("Branch context switched to: feature/gateway-contracts")));
    assert!(sent
        .iter()
        .any(|m| m.contains("Usage budget set to 42.0000")));
    assert!(sent
        .iter()
        .any(|m| m.contains("Usage") && m.contains("user messages")));
    let status = sent
        .iter()
        .rev()
        .find(|m| m.contains("Gateway status"))
        .expect("status reply should exist");
    assert!(status.contains("model: gpt-4o"));
    assert!(status.contains("provider: openai"));
    assert!(status.contains("profile: prod"));
    assert!(status.contains("branch: feature/gateway-contracts"));
    assert!(status.contains("service tier: priority"));
    assert!(sent.iter().any(|m| m.contains("Session title set")));
    assert!(sent.iter().any(|m| m.contains("Resume state")));
    assert!(sent.iter().any(|m| m.contains("Voice mode status")));
    assert!(sent
        .iter()
        .any(|m| m.contains("Update available: Hermes latest")));
    assert!(sent.iter().any(|m| m.contains("Rolled back 2 message")));
}

#[tokio::test]
async fn contract_request_runtime_overrides_reach_context_handler() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let (adapter, state) = ContractAdapter::new("api_server");
    gateway.register_adapter("api_server", adapter).await;
    gateway
        .merge_request_runtime_overrides(
            "api_server",
            "run-42",
            "u1",
            Some("openai:gpt-4o-mini".to_string()),
            Some("openai".to_string()),
            Some("concise".to_string()),
        )
        .await;
    gateway
        .set_message_handler_with_context(Arc::new(|_messages, ctx| {
            Box::pin(async move {
                Ok(format!(
                    "ctx model={:?} provider={:?} personality={:?} session={}",
                    ctx.model, ctx.provider, ctx.personality, ctx.session_key
                ))
            })
        }))
        .await;

    gateway
        .route_message(&IncomingMessage {
            platform: "api_server".to_string(),
            chat_id: "run-42".to_string(),
            user_id: "u1".to_string(),
            text: "run request".to_string(),
            message_id: None,
            is_dm: true,
        })
        .await
        .expect("request override turn should route");

    let sent = state.sent_texts.lock().unwrap().clone();
    let reply = sent.last().expect("context reply");
    assert!(reply.contains("Some(\"openai:gpt-4o-mini\")"));
    assert!(reply.contains("Some(\"openai\")"));
    assert!(reply.contains("Some(\"concise\")"));
    assert!(reply.contains("api_server:run-42"));
}

#[tokio::test]
async fn contract_inline_media_extraction_sends_text_then_images() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;
    gateway
        .set_message_handler(Arc::new(|_messages| {
            Box::pin(async {
                Ok("Here ![diagram](https://cdn.example.com/x.png) and <img src=\"https://fal.media/render/abc\"> done".to_string())
            })
        }))
        .await;

    gateway
        .route_message(&incoming("telegram", "u1", "send image"))
        .await
        .expect("media response should route");

    let sent = state.sent_texts.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0], "Here and done");
    assert_eq!(sent[1], "diagram\nhttps://cdn.example.com/x.png");
    assert_eq!(sent[2], "https://fal.media/render/abc");
}

#[tokio::test]
async fn contract_reconnect_watcher_restarts_offline_primary_adapter() {
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let gateway = Arc::new(Gateway::new(
        session_manager,
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    ));
    let (adapter, state) = ContractAdapter::new("telegram");
    gateway.register_adapter("telegram", adapter).await;

    let watcher_gateway = gateway.clone();
    let watcher = tokio::spawn(async move {
        watcher_gateway.platform_reconnect_watcher(20).await;
    });

    tokio::time::timeout(std::time::Duration::from_secs(22), async {
        loop {
            if state.start_calls.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("watcher should reconnect within one interval");

    assert!(
        state.start_calls.load(Ordering::SeqCst) >= 1,
        "watcher should call start() for offline adapter"
    );
    assert!(state.running.load(Ordering::SeqCst));

    watcher.abort();
}
