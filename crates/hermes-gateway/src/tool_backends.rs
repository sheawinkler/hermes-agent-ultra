//! Real [`MessagingBackend`] / [`ClarifyBackend`] implementations backed by a
//! running [`Gateway`].
//!
//! These bridge the `send_message` and `clarify` agent tools (declared in
//! `hermes-tools`) to the live platform adapters in `hermes-gateway`. When
//! wired at the binary layer, agent tool calls immediately send real
//! messages or block on a real user prompt instead of returning a "pending"
//! envelope.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;
use uuid::Uuid;

use hermes_config::resolve_outbound_media_path;
use hermes_core::ToolError;
use hermes_tools::tools::clarify::ClarifyBackend;
use hermes_tools::tools::messaging::MessagingBackend;

use crate::gateway::Gateway;

/// Default wait for a user response to a `clarify` call. Overridable via
/// `HERMES_CLARIFY_TIMEOUT_SECS`.
const DEFAULT_CLARIFY_TIMEOUT_SECS: u64 = 30;

const SKIP_CHOICE_LABEL: &str = "Skip / 跳过";

fn clarify_async_mode() -> bool {
    matches!(
        std::env::var("HERMES_CLARIFY_ASYNC")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

fn clarify_timeout_secs() -> u64 {
    std::env::var("HERMES_CLARIFY_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CLARIFY_TIMEOUT_SECS)
}

fn with_skip_choice(choices: Option<&[String]>) -> Vec<String> {
    let mut out: Vec<String> = choices.map(|c| c.to_vec()).unwrap_or_default();
    if !out.iter().any(|c| c.contains("Skip") || c.contains("跳过")) {
        out.push(SKIP_CHOICE_LABEL.to_string());
    }
    out
}

// ---------------------------------------------------------------------------
// GatewayMessagingBackend
// ---------------------------------------------------------------------------

/// [`MessagingBackend`] that forwards to a live [`Gateway::send_message`].
pub struct GatewayMessagingBackend {
    gateway: Arc<Gateway>,
}

impl GatewayMessagingBackend {
    pub fn new(gateway: Arc<Gateway>) -> Self {
        Self { gateway }
    }
}

#[async_trait]
impl MessagingBackend for GatewayMessagingBackend {
    async fn send(
        &self,
        platform: &str,
        recipient: &str,
        message: &str,
    ) -> Result<String, ToolError> {
        self.gateway
            .send_message(platform, recipient, message, None)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("gateway send failed: {}", e)))?;
        Ok(json!({
            "type": "messaging_result",
            "platform": platform,
            "recipient": recipient,
            "status": "sent",
        })
        .to_string())
    }

    async fn send_file(
        &self,
        platform: &str,
        recipient: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<String, ToolError> {
        let resolved = resolve_outbound_media_path(file_path)
            .map_err(ToolError::ExecutionFailed)?;
        let path_str = resolved.to_string_lossy().into_owned();
        self.gateway
            .send_file(platform, recipient, &path_str, caption)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("gateway send_file failed: {e}")))?;
        Ok(json!({
            "type": "messaging_result",
            "platform": platform,
            "recipient": recipient,
            "status": "sent",
            "file": path_str,
            "resolved_path": path_str,
        })
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// ChannelClarifyBackend
// ---------------------------------------------------------------------------

pub struct ChannelClarifyBackend {
    dispatcher: ClarifyDispatcher,
}

impl ChannelClarifyBackend {
    pub fn new(dispatcher: ClarifyDispatcher) -> Self {
        Self { dispatcher }
    }
}

#[async_trait]
impl ClarifyBackend for ChannelClarifyBackend {
    async fn ask(&self, question: &str, choices: Option<&[String]>) -> Result<String, ToolError> {
        let choices_vec = with_skip_choice(choices);
        let id = self
            .dispatcher
            .register(question, &choices_vec)
            .await;

        if clarify_async_mode() {
            return Ok(json!({
                "type": "clarify_pending",
                "status": "pending",
                "clarification_id": id,
                "question": question,
                "choices": choices_vec,
                "hint": "Reply in chat to answer, or choose Skip. Match clarification_id when resuming."
            })
            .to_string());
        }

        let wait_secs = clarify_timeout_secs();
        match self.dispatcher.wait_for(&id, wait_secs).await {
            Ok(answer) => Ok(json!({
                "type": "clarify_response",
                "clarification_id": id,
                "question": question,
                "answer": answer,
            })
            .to_string()),
            Err(_) => Ok(json!({
                "type": "clarify_response",
                "clarification_id": id,
                "question": question,
                "answer": null,
                "timed_out": true,
                "hint": "User did not respond within the timeout. Proceed with a reasonable default.",
            })
            .to_string()),
        }
    }
}

/// A pending clarify request awaiting a user response.
pub struct PendingClarify {
    pub id: String,
    pub question: String,
    pub choices: Vec<String>,
}

impl PendingClarify {
    pub async fn respond(self, dispatcher: &ClarifyDispatcher, answer: impl Into<String>) -> Result<(), String> {
        dispatcher.respond_by_id(&self.id, answer).await
    }
}

/// Handle shared between the backend (producer) and the UI layer (consumer).
#[derive(Clone, Default)]
pub struct ClarifyDispatcher {
    queue: Arc<Mutex<Vec<PendingClarify>>>,
    senders: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
    receivers: Arc<Mutex<HashMap<String, oneshot::Receiver<String>>>>,
}

impl ClarifyDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    async fn register(&self, question: &str, choices: &[String]) -> String {
        let id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel::<String>();
        self.senders.lock().await.insert(id.clone(), tx);
        self.receivers.lock().await.insert(id.clone(), rx);
        self.queue.lock().await.push(PendingClarify {
            id: id.clone(),
            question: question.to_string(),
            choices: choices.to_vec(),
        });
        id
    }

    async fn wait_for(&self, id: &str, wait_secs: u64) -> Result<String, String> {
        let rx = self
            .receivers
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| format!("no pending clarify with id {id}"))?;
        match timeout(Duration::from_secs(wait_secs), rx).await {
            Ok(Ok(answer)) => {
                self.senders.lock().await.remove(id);
                Ok(answer)
            }
            Ok(Err(_)) => Err("clarify responder dropped without answering".into()),
            Err(_) => Err(format!("clarify timed out after {}s", wait_secs)),
        }
    }

    /// Remove and return the oldest pending request, if any.
    pub async fn take_next(&self) -> Option<PendingClarify> {
        let mut guard = self.queue.lock().await;
        if guard.is_empty() {
            None
        } else {
            Some(guard.remove(0))
        }
    }

    /// Fulfill a pending clarify by id.
    pub async fn respond_by_id(&self, id: &str, answer: impl Into<String>) -> Result<(), String> {
        let sender = self
            .senders
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| format!("no pending clarify with id {id}"))?;
        self.receivers.lock().await.remove(id);
        sender
            .send(answer.into())
            .map_err(|_| "clarify backend was dropped".to_string())
    }

    pub async fn pending(&self) -> usize {
        self.queue.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env;

    #[tokio::test]
    async fn dispatcher_roundtrip() {
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());

        test_env::set_var("HERMES_CLARIFY_TIMEOUT_SECS", "5");
        test_env::remove_var("HERMES_CLARIFY_ASYNC");

        let ask = tokio::spawn(async move {
            backend
                .ask("pick one?", Some(&["a".into(), "b".into()]))
                .await
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        let pending = dispatcher
            .take_next()
            .await
            .expect("expected one pending clarify");
        assert_eq!(pending.question, "pick one?");
        assert!(pending.choices.iter().any(|c| c.contains("Skip")));
        pending.respond(&dispatcher, "a").await.unwrap();

        let answer = ask.await.unwrap().unwrap();
        assert!(answer.contains("\"answer\":\"a\""));
    }

    #[tokio::test]
    async fn dispatcher_times_out() {
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());
        test_env::set_var("HERMES_CLARIFY_TIMEOUT_SECS", "1");
        test_env::remove_var("HERMES_CLARIFY_ASYNC");

        let out = backend.ask("q?", None).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["timed_out"], true);
        assert!(parsed["answer"].is_null());
    }

    #[tokio::test]
    async fn clarify_async_returns_pending_without_blocking() {
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());
        test_env::set_var("HERMES_CLARIFY_ASYNC", "1");

        let out = backend.ask("choose?", Some(&["x".into()])).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "pending");
        let id = parsed["clarification_id"].as_str().unwrap().to_string();
        dispatcher.respond_by_id(&id, "x").await.unwrap();
    }
}
