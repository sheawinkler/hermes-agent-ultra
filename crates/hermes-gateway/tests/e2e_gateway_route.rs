use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_config::session::SessionConfig;
use hermes_core::GatewayError;
use hermes_gateway::dm::DmManager;
use hermes_gateway::gateway::{GatewayConfig, IncomingMessage};
use hermes_gateway::{Gateway, ParseMode, PlatformAdapter, SessionManager};

struct RecordingAdapter {
    sent: Arc<Mutex<Vec<String>>>,
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
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.sent.lock().unwrap().push(text.to_string());
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
        "test"
    }
}

#[tokio::test]
async fn e2e_gateway_routes_message_and_replies() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(RecordingAdapter { sent: sent.clone() });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("u1");
    let gateway = Gateway::new(session_manager, dm, GatewayConfig::default());
    gateway.register_adapter("test", adapter).await;
    gateway
        .set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let user_count = messages
                    .iter()
                    .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
                    .count();
                Ok(format!("ack users={}", user_count))
            })
        }))
        .await;

    gateway
        .route_message(&IncomingMessage {
            platform: "test".to_string(),
            chat_id: "c1".to_string(),
            user_id: "u1".to_string(),
            text: "hello".to_string(),
            message_id: None,
            is_dm: true,
        })
        .await
        .expect("route should succeed");

    let output = sent.lock().unwrap().clone();
    assert!(output.iter().any(|s| s.contains("ack users=1")));
}
