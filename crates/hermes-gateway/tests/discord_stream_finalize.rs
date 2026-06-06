//! Discord streaming finalize: no duplicate fallback when final edit fails.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_gateway::platforms::discord::stream_finalize::deliver_legacy_stream_final;

struct MockDiscordAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    delete_ok: bool,
}

#[async_trait]
impl PlatformAdapter for MockDiscordAdapter {
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
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        _message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        if text == "…" {
            let mut msgs = self.messages.lock().unwrap();
            if let Some(pos) = msgs
                .iter()
                .position(|(c, t)| c == chat_id && t == "partial-stream")
            {
                msgs[pos].1 = text.to_string();
                return Ok(());
            }
        }
        Err(GatewayError::SendFailed(
            "{\"message\": \"You are being rate limited.\", \"retry_after\": 0.01}".into(),
        ))
    }

    async fn delete_message(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Result<bool, GatewayError> {
        if !self.delete_ok {
            return Err(GatewayError::SendFailed("delete failed".into()));
        }
        let mut msgs = self.messages.lock().unwrap();
        msgs.retain(|(c, t)| !(c == chat_id && t == "partial-stream"));
        let _ = message_id;
        Ok(true)
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
async fn final_edit_failure_deletes_placeholder_before_resend() {
    let messages = Arc::new(Mutex::new(vec![(
        "ch1".to_string(),
        "partial-stream".to_string(),
    )]));
    let adapter = MockDiscordAdapter {
        messages: messages.clone(),
        delete_ok: true,
    };

    let chunks = vec!["final body".to_string()];
    deliver_legacy_stream_final(
        &adapter,
        "discord",
        "ch1",
        Some("msg-1"),
        &chunks,
    )
    .await
    .expect("delivery");

    let out = messages.lock().unwrap();
    assert_eq!(out.len(), 1, "expected one message after delete+send, got {:?}", out);
    assert_eq!(out[0].1, "final body");
}

#[tokio::test]
async fn final_edit_failure_without_delete_clears_placeholder() {
    let messages = Arc::new(Mutex::new(vec![(
        "ch1".to_string(),
        "partial-stream".to_string(),
    )]));
    let adapter = MockDiscordAdapter {
        messages: messages.clone(),
        delete_ok: false,
    };

    let chunks = vec!["final body".to_string()];
    deliver_legacy_stream_final(
        &adapter,
        "discord",
        "ch1",
        Some("msg-1"),
        &chunks,
    )
    .await
    .expect("delivery");

    let out = messages.lock().unwrap();
    assert_eq!(out.len(), 2, "placeholder cleared + new message: {:?}", out);
    assert_eq!(out[0].1, "…");
    assert_eq!(out[1].1, "final body");
}
