//! Real [`MessagingBackend`] / [`ClarifyBackend`] implementations backed by a
//! running [`Gateway`].
//!
//! These bridge the `send_message` and `clarify` agent tools (declared in
//! `hermes-tools`) to the live platform adapters in `hermes-gateway`. When
//! wired at the binary layer, agent tool calls immediately send real
//! messages or block on a real user prompt instead of returning a "pending"
//! envelope.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;

use hermes_core::ToolError;
use hermes_tools::tools::clarify::ClarifyBackend;
use hermes_tools::tools::messaging::MessagingBackend;

use crate::gateway::Gateway;

// ---------------------------------------------------------------------------
// GatewayMessagingBackend
// ---------------------------------------------------------------------------

/// [`MessagingBackend`] that forwards to a live [`Gateway::send_message`].
///
/// `platform` must match a registered adapter (`telegram`, `discord`,
/// `slack`, `email`, ...) — otherwise the underlying gateway returns an
/// error which is propagated as [`ToolError::ExecutionFailed`].
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
}

// ---------------------------------------------------------------------------
// ChannelClarifyBackend
// ---------------------------------------------------------------------------

/// Default wait for a user response to a `clarify` call. Overridable via
/// `HERMES_CLARIFY_TIMEOUT_SECS`.
const DEFAULT_CLARIFY_TIMEOUT_SECS: u64 = 300;

/// [`ClarifyBackend`] that blocks the tool call on a tokio `oneshot`
/// receiver, letting the TUI / gateway respond asynchronously.
///
/// The caller side owns a cloned [`ClarifyDispatcher`] which it uses to
/// fulfil the pending request (see [`ClarifyDispatcher::respond`]). If no
/// response arrives within the configured timeout, the tool returns an
/// `ExecutionFailed` error so the agent loop can retry or abandon.
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
        let (tx, rx) = oneshot::channel::<String>();
        let req = PendingClarify {
            question: question.to_string(),
            choices: choices.map(|c| c.to_vec()).unwrap_or_default(),
            responder: tx,
        };

        self.dispatcher.enqueue(req).await;

        let wait_secs = std::env::var("HERMES_CLARIFY_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_CLARIFY_TIMEOUT_SECS);

        match timeout(Duration::from_secs(wait_secs), rx).await {
            Ok(Ok(answer)) => Ok(json!({
                "type": "clarify_response",
                "question": question,
                "answer": answer,
            })
            .to_string()),
            Ok(Err(_)) => Err(ToolError::ExecutionFailed(
                "clarify responder dropped without answering".into(),
            )),
            Err(_) => Err(ToolError::ExecutionFailed(format!(
                "clarify timed out after {}s",
                wait_secs
            ))),
        }
    }
}

/// A pending clarify request awaiting a user response.
pub struct PendingClarify {
    pub question: String,
    pub choices: Vec<String>,
    responder: oneshot::Sender<String>,
}

impl PendingClarify {
    pub fn respond(self, answer: impl Into<String>) -> Result<(), String> {
        self.responder
            .send(answer.into())
            .map_err(|_| "clarify backend was dropped".to_string())
    }
}

/// Handle shared between the backend (producer) and the UI layer (consumer).
///
/// Internally it holds a mutex-guarded queue of [`PendingClarify`] requests;
/// the UI/TUI drains them via [`ClarifyDispatcher::take_next`] and calls
/// [`PendingClarify::respond`] when the user has answered.
#[derive(Clone, Default)]
pub struct ClarifyDispatcher {
    inner: Arc<Mutex<Vec<PendingClarify>>>,
}

impl ClarifyDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    async fn enqueue(&self, req: PendingClarify) {
        self.inner.lock().await.push(req);
    }

    /// Remove and return the oldest pending request, if any.
    pub async fn take_next(&self) -> Option<PendingClarify> {
        let mut guard = self.inner.lock().await;
        if guard.is_empty() {
            None
        } else {
            Some(guard.remove(0))
        }
    }

    /// Current queue depth. Intended for diagnostics / status bars.
    pub async fn pending(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatcher_roundtrip() {
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());

        std::env::set_var("HERMES_CLARIFY_TIMEOUT_SECS", "5");

        let ask = tokio::spawn(async move {
            backend
                .ask("pick one?", Some(&["a".into(), "b".into()]))
                .await
        });

        // Give the producer a moment to enqueue.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let pending = dispatcher
            .take_next()
            .await
            .expect("expected one pending clarify");
        assert_eq!(pending.question, "pick one?");
        pending.respond("a").unwrap();

        let answer = ask.await.unwrap().unwrap();
        assert!(answer.contains("\"answer\":\"a\""));
    }

    #[tokio::test]
    async fn dispatcher_times_out() {
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());
        std::env::set_var("HERMES_CLARIFY_TIMEOUT_SECS", "1");

        let err = backend.ask("q?", None).await.unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }
}
