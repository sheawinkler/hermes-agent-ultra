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
        message_id: None,
        is_dm: true,
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
