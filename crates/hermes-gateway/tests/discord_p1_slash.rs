//! Discord P1 slash command routing (interaction follow-up vs channel send).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_config::session::SessionConfig;
use hermes_core::GatewayError;
use hermes_gateway::dm::DmManager;
use hermes_gateway::gateway::{Gateway, GatewayConfig, IncomingMessage};
use hermes_gateway::{ParseMode, PlatformAdapter, SessionManager};

struct InteractionRecordingAdapter {
    channel_sent: Arc<Mutex<Vec<String>>>,
    interaction_replies: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl PlatformAdapter for InteractionRecordingAdapter {
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
        self.channel_sent.lock().unwrap().push(text.to_string());
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

    async fn respond_interaction(
        &self,
        _interaction_id: &str,
        _interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        self.interaction_replies
            .lock()
            .unwrap()
            .push(content.to_string());
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "discord"
    }
}

#[tokio::test]
async fn slash_status_replies_via_interaction_not_channel() {
    let channel_sent = Arc::new(Mutex::new(Vec::new()));
    let interaction_replies = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(InteractionRecordingAdapter {
        channel_sent: channel_sent.clone(),
        interaction_replies: interaction_replies.clone(),
    });

    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_manager,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.register_adapter("discord", adapter).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "ch-slash".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        media_urls: vec![],
        media_types: vec![],
        message_id: None,
        is_dm: false,
        interaction_id: Some("interaction-1".into()),
        interaction_token: Some("interaction-token".into()),
        role_ids: vec![],
    };

    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(
        channel_sent.lock().unwrap().is_empty(),
        "slash follow-up must not use channel send_message"
    );
    assert!(
        !interaction_replies.lock().unwrap().is_empty(),
        "slash command should respond via interaction callback"
    );
}
