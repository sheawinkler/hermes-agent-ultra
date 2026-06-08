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
use tracing::{debug, warn};
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

/// Pull a numeric choice out of IM replies like `@bot 2`.
pub(crate) fn extract_clarify_choice_token(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(last) = trimmed.split_whitespace().last() {
        if last.parse::<usize>().is_ok() {
            return last.to_string();
        }
    }
    trimmed.to_string()
}

/// By default, gateway clarify blocks up to [`DEFAULT_CLARIFY_TIMEOUT_SECS`] for a
/// reply. Set `HERMES_CLARIFY_ASYNC=1` to return immediately and accept the
/// answer on a later inbound message instead.
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
    async fn ask(
        &self,
        question: &str,
        choices: Option<&[String]>,
        session_key: Option<&str>,
    ) -> Result<String, ToolError> {
        let choices_vec = with_skip_choice(choices);
        let async_mode = clarify_async_mode();
        let wait_secs = clarify_timeout_secs();
        debug!(
            question = %question,
            choice_count = choices_vec.len(),
            choices = ?choices_vec,
            session_key = ?session_key,
            async_mode,
            wait_secs,
            "channel clarify: registering pending request"
        );
        let id = self
            .dispatcher
            .register(question, &choices_vec, session_key)
            .await;
        let queue_depth = self.dispatcher.pending().await;
        debug!(
            clarification_id = %id,
            queue_depth,
            "channel clarify: registered; awaiting user reply on a future inbound message"
        );

        if async_mode {
            debug!(
                clarification_id = %id,
                "channel clarify: async mode — returning clarify_pending without blocking"
            );
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

        debug!(
            clarification_id = %id,
            wait_secs,
            "channel clarify: blocking until user responds or timeout"
        );
        match self.dispatcher.wait_for(&id, wait_secs).await {
            Ok(answer) => {
                debug!(
                    clarification_id = %id,
                    answer_len = answer.len(),
                    "channel clarify: received user answer"
                );
                Ok(json!({
                    "type": "clarify_response",
                    "clarification_id": id,
                    "question": question,
                    "answer": answer,
                })
                .to_string())
            }
            Err(e) => {
                warn!(
                    clarification_id = %id,
                    wait_secs,
                    error = %e,
                    "channel clarify: timed out or failed while waiting for user answer"
                );
                Ok(json!({
                    "type": "clarify_response",
                    "clarification_id": id,
                    "question": question,
                    "answer": null,
                    "timed_out": true,
                    "hint": "User did not respond within the timeout. Proceed with a reasonable default.",
                })
                .to_string())
            }
        }
    }
}

/// A pending clarify request awaiting a user response.
pub struct PendingClarify {
    pub id: String,
    pub question: String,
    pub choices: Vec<String>,
    pub session_key: Option<String>,
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
    /// Session with a clarify currently blocked in `wait_for` (sync IM mode).
    active_session_waits: Arc<Mutex<HashMap<String, String>>>,
}

impl ClarifyDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    async fn register(&self, question: &str, choices: &[String], session_key: Option<&str>) -> String {
        let id = Uuid::new_v4().to_string();
        let session_key = session_key
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let (tx, rx) = oneshot::channel::<String>();
        self.senders.lock().await.insert(id.clone(), tx);
        self.receivers.lock().await.insert(id.clone(), rx);
        self.queue.lock().await.push(PendingClarify {
            id: id.clone(),
            question: question.to_string(),
            choices: choices.to_vec(),
            session_key: session_key.clone(),
        });
        if let Some(sk) = session_key.as_deref() {
            self.active_session_waits
                .lock()
                .await
                .insert(sk.to_string(), id.clone());
        }
        debug!(
            clarification_id = %id,
            question = %question,
            choice_count = choices.len(),
            session_key = ?session_key,
            "clarify dispatcher: registered pending request"
        );
        id
    }

    async fn finish_pending(&self, id: &str) {
        self.senders.lock().await.remove(id);
        self.receivers.lock().await.remove(id);
        self.remove_from_queue(id).await;
        let mut waits = self.active_session_waits.lock().await;
        waits.retain(|_, v| v != id);
    }

    async fn remove_from_queue(&self, id: &str) {
        let mut guard = self.queue.lock().await;
        if let Some(pos) = guard.iter().position(|p| p.id == id) {
            guard.remove(pos);
        }
    }

    async fn normalize_answer(&self, id: &str, raw: &str) -> String {
        let trimmed = extract_clarify_choice_token(raw);
        let choices = self
            .queue
            .lock()
            .await
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.choices.clone());
        if let Some(choices) = choices {
            if let Ok(n) = trimmed.parse::<usize>() {
                if (1..=choices.len()).contains(&n) {
                    return choices[n - 1].clone();
                }
            }
        }
        trimmed.to_string()
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
                self.finish_pending(id).await;
                debug!(
                    clarification_id = %id,
                    answer_len = answer.len(),
                    "clarify dispatcher: wait_for fulfilled"
                );
                Ok(answer)
            }
            Ok(Err(_)) => {
                self.finish_pending(id).await;
                warn!(
                    clarification_id = %id,
                    "clarify dispatcher: responder dropped without answering"
                );
                Err("clarify responder dropped without answering".into())
            }
            Err(_) => {
                self.finish_pending(id).await;
                warn!(
                    clarification_id = %id,
                    wait_secs,
                    "clarify dispatcher: wait_for timed out"
                );
                Err(format!("clarify timed out after {}s", wait_secs))
            }
        }
    }

    /// Fulfill the active sync clarify for `session_key` without acquiring the
    /// per-session route lock. Used when the user replies while an agent turn is
    /// blocked in `wait_for`.
    pub async fn try_fulfill_for_session(&self, session_key: &str, raw_answer: &str) -> bool {
        if clarify_async_mode() {
            return false;
        }
        let id = {
            let waits = self.active_session_waits.lock().await;
            waits.get(session_key).cloned()
        };
        let Some(id) = id else {
            debug!(
                session_key = %session_key,
                "clarify dispatcher: fast-path skipped — no active session wait"
            );
            return false;
        };
        if !self.senders.lock().await.contains_key(&id) {
            debug!(
                clarification_id = %id,
                session_key = %session_key,
                "clarify dispatcher: fast-path skipped — no active sender"
            );
            return false;
        }
        let answer = self.normalize_answer(&id, raw_answer).await;
        match self.respond_by_id(&id, answer).await {
            Ok(()) => {
                debug!(
                    clarification_id = %id,
                    session_key = %session_key,
                    "clarify dispatcher: fast-path fulfilled active session wait"
                );
                true
            }
            Err(e) => {
                debug!(
                    clarification_id = %id,
                    session_key = %session_key,
                    error = %e,
                    "clarify dispatcher: fast-path fulfill failed"
                );
                false
            }
        }
    }

    /// Remove and return the oldest pending request, if any.
    pub async fn take_next(&self) -> Option<PendingClarify> {
        let mut guard = self.queue.lock().await;
        if guard.is_empty() {
            None
        } else {
            let pending = guard.remove(0);
            debug!(
                clarification_id = %pending.id,
                question = %pending.question,
                choice_count = pending.choices.len(),
                remaining_queue = guard.len(),
                "clarify dispatcher: took next pending request"
            );
            Some(pending)
        }
    }

    /// Fulfill a pending clarify by id.
    pub async fn respond_by_id(&self, id: &str, answer: impl Into<String>) -> Result<(), String> {
        let answer = answer.into();
        let sender = self
            .senders
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| format!("no pending clarify with id {id}"))?;
        debug!(
            clarification_id = %id,
            answer_len = answer.len(),
            "clarify dispatcher: respond_by_id"
        );
        sender
            .send(answer)
            .map_err(|_| "clarify backend was dropped".to_string())?;
        self.receivers.lock().await.remove(id);
        Ok(())
    }

    pub async fn pending(&self) -> usize {
        self.queue.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env;
    use std::sync::LazyLock;
    use tokio::sync::Mutex as AsyncMutex;

    static CLARIFY_ENV_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

    #[tokio::test]
    async fn dispatcher_roundtrip() {
        let _env_guard = CLARIFY_ENV_LOCK.lock().await;
        test_env::set_var("HERMES_CLARIFY_TIMEOUT_SECS", "5");
        test_env::remove_var("HERMES_CLARIFY_ASYNC");
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());

        let ask = tokio::spawn(async move {
            backend
                .ask("pick one?", Some(&["a".into(), "b".into()]), None)
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
        let _env_guard = CLARIFY_ENV_LOCK.lock().await;
        test_env::set_var("HERMES_CLARIFY_TIMEOUT_SECS", "1");
        test_env::remove_var("HERMES_CLARIFY_ASYNC");
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());

        let out = backend.ask("q?", None, None).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["timed_out"], true);
        assert!(parsed["answer"].is_null());
    }

    #[tokio::test]
    async fn clarify_async_returns_pending_without_blocking() {
        let _env_guard = CLARIFY_ENV_LOCK.lock().await;
        test_env::set_var("HERMES_CLARIFY_ASYNC", "1");
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());

        let out = backend.ask("choose?", Some(&["x".into()]), None).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "pending");
        let id = parsed["clarification_id"].as_str().unwrap().to_string();
        dispatcher.respond_by_id(&id, "x").await.unwrap();
    }

    #[tokio::test]
    async fn try_fulfill_for_session_while_sync_wait() {
        let _env_guard = CLARIFY_ENV_LOCK.lock().await;
        test_env::remove_var("HERMES_CLARIFY_ASYNC");
        test_env::set_var("HERMES_CLARIFY_TIMEOUT_SECS", "30");
        let dispatcher = ClarifyDispatcher::new();
        let backend = ChannelClarifyBackend::new(dispatcher.clone());
        let session = "wecom:test-chat";

        let ask = tokio::spawn(async move {
            backend
                .ask(
                    "pick one?",
                    Some(&["alpha".into(), "beta".into()]),
                    Some(session),
                )
                .await
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            dispatcher
                .try_fulfill_for_session(session, "@hermes-bot 2")
                .await,
            "fast-path should fulfill active sync clarify"
        );

        let answer = ask.await.unwrap().unwrap();
        assert!(answer.contains("beta"), "numeric choice 2 should map to beta");
    }

    #[test]
    fn extract_clarify_choice_token_parses_mention_reply() {
        assert_eq!(extract_clarify_choice_token("@李智杨的hermes 2"), "2");
        assert_eq!(extract_clarify_choice_token("  3 "), "3");
    }
}
