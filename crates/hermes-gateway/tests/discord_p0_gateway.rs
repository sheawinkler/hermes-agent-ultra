//! Discord P0 gateway integration tests (I-01 .. I-05).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_config::session::SessionConfig;
use hermes_core::GatewayError;
use hermes_gateway::dm::DmManager;
use hermes_gateway::gateway::{
    Gateway, GatewayConfig, GroupAccessMode, IncomingMessage, PlatformAccessPolicy,
};
use hermes_gateway::{ParseMode, PlatformAdapter, SessionManager};

struct RecordingAdapter {
    sent: Arc<Mutex<Vec<(String, String)>>>,
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
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.sent
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
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
        "discord"
    }
}

#[tokio::test]
async fn i01_guild_allowlist_user_gets_reply() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(RecordingAdapter { sent: sent.clone() });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Arc::new(Gateway::new(
        session_manager,
        DmManager::with_ignore_behavior(),
        GatewayConfig::default(),
    ));
    gw.register_adapter("discord", adapter.clone()).await;
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("allowed_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    gw.set_message_handler(Arc::new(|_| {
        Box::pin(async { Ok("pong".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild-ch".into(),
        user_id: "allowed_user".into(),
        text: "ping".into(),
        media_urls: vec![],
        media_types: vec![],
        message_id: Some("m1".into()),
        is_dm: false,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    let sent = adapter.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "guild-ch");
}

#[tokio::test]
async fn i02_guild_denied_user_no_transcript() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(RecordingAdapter { sent: sent.clone() });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Arc::new(Gateway::new(
        session_manager,
        DmManager::with_ignore_behavior(),
        GatewayConfig::default(),
    ));
    gw.register_adapter("discord", adapter).await;
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("allowed_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild-ch".into(),
        user_id: "other_user".into(),
        text: "ping".into(),
        media_urls: vec![],
        media_types: vec![],
        message_id: Some("m2".into()),
        is_dm: false,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "guild-ch", "other_user")
            .await,
        0
    );
}

#[tokio::test]
async fn i03_dm_authorized_user_gets_reply() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(RecordingAdapter { sent: sent.clone() });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_ignore_behavior();
    dm.authorize_user("dm_user");
    let gw = Arc::new(Gateway::new(
        session_manager,
        dm,
        GatewayConfig::default(),
    ));
    gw.register_adapter("discord", adapter.clone()).await;
    gw.set_message_handler(Arc::new(|_| {
        Box::pin(async { Ok("dm-reply".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "dm-ch".into(),
        user_id: "dm_user".into(),
        text: "hello".into(),
        media_urls: vec![],
        media_types: vec![],
        message_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(!adapter.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn i04_dm_unauthorized_silent() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(RecordingAdapter { sent });
    let session_manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Arc::new(Gateway::new(
        session_manager,
        DmManager::with_ignore_behavior(),
        GatewayConfig::default(),
    ));
    gw.register_adapter("discord", adapter.clone()).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "dm-ch".into(),
        user_id: "stranger".into(),
        text: "hello".into(),
        media_urls: vec![],
        media_types: vec![],
        message_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(adapter.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn i05_session_key_per_user_in_shared_channel() {
    let session_manager = Arc::new(SessionManager::with_group_isolation(
        SessionConfig::default(),
        true,
    ));
    // Discord channel ids are numeric snowflakes; group isolation uses Group session type.
    let key_a = session_manager.compose_session_key_with_dm(
        "discord",
        "1234567890",
        "alice",
        Some(false),
    );
    let key_b = session_manager.compose_session_key_with_dm(
        "discord",
        "1234567890",
        "bob",
        Some(false),
    );
    assert_ne!(key_a, key_b);
    assert_eq!(key_a, "discord:1234567890:alice");
    assert_eq!(key_b, "discord:1234567890:bob");
}
