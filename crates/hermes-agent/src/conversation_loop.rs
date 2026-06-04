//! Python `agent.conversation_loop.run_conversation` parity — single entry for one user turn.
//!
//! (`prepare_turn`): sanitize, append user, hydrate counters, message prelude.
//! delegated to [`AgentLoop::run_prepared`] / [`AgentLoop::run_stream_prepared`].
//! (`finalize_turn`): turn-level hooks, `ConversationResult` assembly, optional persist.

use hermes_core::{AgentError, AgentResult, Message, MessageRole, StreamChunk, ToolSchema};

use crate::agent_loop::AgentLoop;
use crate::message_sanitization::{sanitize_surrogates, strip_system_messages_from_history};
use crate::plugins::{HookResult, HookType};
use crate::session_persistence::leading_system_prompt_for_persist;

/// Inputs for one `run_conversation` call (Python `run_conversation` kwargs).
pub struct RunConversationParams {
    pub user_message: String,
    pub conversation_history: Vec<Message>,
    pub task_id: Option<String>,
    pub stream_callback: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
    pub persist_user_message: Option<String>,
    pub tools: Option<Vec<ToolSchema>>,
    /// When set, persist session messages to SQLite after the turn (gateway/HTTP parity).
    pub persist_session: bool,
}

/// Python `run_conversation` return dict (subset + full [`AgentResult`]).
#[derive(Debug, Clone)]
pub struct ConversationResult {
    pub final_response: Option<String>,
    pub last_reasoning: Option<String>,
    pub messages: Vec<Message>,
    pub completed: bool,
    pub interrupted: bool,
    pub pending_steer: Option<String>,
    pub inner: AgentResult,
}

/// Metadata carried from B segment through E segment.
#[derive(Debug, Clone)]
pub struct TurnFinalizeMeta {
    pub original_user_message: String,
    pub task_id: String,
    pub persist_session: bool,
}

/// Output of B-segment [`AgentLoop::prepare_turn`].
#[derive(Debug, Clone)]
pub struct PreparedTurn {
    pub meta: TurnFinalizeMeta,
    pub messages: Vec<Message>,
}

/// Split gateway/session messages into prior history and the current user turn text.
///
/// Session storage includes the inbound user line; `run_conversation` appends it again in
/// `prepare_turn`, so callers must pass history **without** the current turn's user message,
/// or use this helper on the full session slice.
pub fn split_messages_for_run_conversation(messages: &[Message]) -> Option<(Vec<Message>, String)> {
    let messages = strip_system_messages_from_history(messages);
    let idx = messages.iter().rposition(|m| m.role == MessageRole::User)?;
    let user_message = messages[idx].content.clone().unwrap_or_default();
    let mut history = messages;
    history.remove(idx);
    Some((history, user_message))
}

/// Last assistant visible text in the message list.
pub fn extract_last_assistant_reply(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|m| {
        if m.role == MessageRole::Assistant {
            m.content.clone().filter(|c| !c.trim().is_empty())
        } else {
            None
        }
    })
}

/// Reasoning from the current turn only (stop at the prior user boundary).
pub fn extract_last_reasoning_current_turn(messages: &[Message]) -> Option<String> {
    for msg in messages.iter().rev() {
        if msg.role == MessageRole::User {
            break;
        }
        if msg.role == MessageRole::Assistant {
            if let Some(ref r) = msg.reasoning_content {
                if !r.trim().is_empty() {
                    return Some(r.clone());
                }
            }
        }
    }
    None
}

impl AgentLoop {
    /// Run one full user turn (Python `run_conversation`).
    pub async fn run_conversation(
        &self,
        params: RunConversationParams,
    ) -> Result<ConversationResult, AgentError> {
        let prepared = self.prepare_turn(&params).await?;
        let tools = params.tools;
        let inner = if let Some(cb) = params.stream_callback {
            self.run_stream_prepared(prepared.messages.clone(), tools, Some(cb))
                .await?
        } else {
            self.run_prepared(prepared.messages.clone(), tools).await?
        };
        Ok(self.finalize_turn(inner, &prepared.meta))
    }

    /// build message list and apply per-turn prelude.
    pub(crate) async fn prepare_turn(
        &self,
        params: &RunConversationParams,
    ) -> Result<PreparedTurn, AgentError> {
        let mut user_message = params.user_message.clone();
        user_message = sanitize_surrogates(&user_message).into_owned();

        let persist_override = params
            .persist_user_message
            .as_ref()
            .map(|s| sanitize_surrogates(s).into_owned());

        let original_user_message = persist_override
            .clone()
            .unwrap_or_else(|| user_message.clone());

        {
            let mut cfg = self
                .config_runtime
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cfg.persist_user_message = persist_override;
        }

        let task_id = params
            .task_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Ok(mut slot) = self.current_task_id.lock() {
            *slot = Some(task_id.clone());
        }

        let conversation_history = strip_system_messages_from_history(&params.conversation_history);
        self.hydrate_memory_nudge_counters_from_history(&conversation_history);

        let mut messages: Vec<Message> = conversation_history;
        messages.push(Message::user(user_message));

        self.apply_turn_message_prelude(&mut messages).await;

        Ok(PreparedTurn {
            meta: TurnFinalizeMeta {
                original_user_message,
                task_id,
                persist_session: params.persist_session,
            },
            messages,
        })
    }

    /// turn-level hooks + [`ConversationResult`] + optional session persist.
    pub(crate) fn finalize_turn(
        &self,
        mut inner: AgentResult,
        meta: &TurnFinalizeMeta,
    ) -> ConversationResult {
        let messages = inner.messages.clone();
        let mut final_response = extract_last_assistant_reply(&messages);
        let last_reasoning = extract_last_reasoning_current_turn(&messages);
        let interrupted = inner.interrupted;
        let completed = inner.finished_naturally && !interrupted;

        if let Some(ref mut text) = final_response {
            if !interrupted {
                *text = self.apply_turn_level_output_hooks(text, meta, &messages);
            }
        }

        if meta.persist_session {
            self.persist_turn_session(&messages);
        }

        inner.messages = messages.clone();
        let inner = self.finalize_agent_result(inner);

        ConversationResult {
            final_response: final_response.clone(),
            last_reasoning,
            messages,
            completed,
            interrupted,
            pending_steer: inner.pending_steer.clone(),
            inner,
        }
    }

    fn hydrate_memory_nudge_counters_from_history(&self, history: &[Message]) {
        if history.is_empty() {
            return;
        }
        let interval = self.config().memory_nudge_interval;
        if interval == 0 {
            return;
        }
        let prior_user_turns = history
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count();
        if prior_user_turns == 0 {
            return;
        }
        if let Ok(mut c) = self.evolution_counters.lock() {
            if c.turns_since_memory == 0 {
                c.turns_since_memory = (prior_user_turns % interval as usize) as u32;
            }
        }
    }

    fn apply_turn_level_output_hooks(
        &self,
        response: &str,
        meta: &TurnFinalizeMeta,
        messages: &[Message],
    ) -> String {
        let history_json: Vec<serde_json::Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let hook_ctx = serde_json::json!({
            "session_id": self.config().session_id,
            "user_message": meta.original_user_message,
            "assistant_response": response,
            "conversation_history": history_json,
            "model": self.active_model(),
            "platform": self.config().platform,
            "task_id": meta.task_id,
        });
        let results = self.invoke_hook(HookType::PostLlmCall, &hook_ctx);
        let mut out = response.to_string();
        for r in results {
            if let HookResult::TransformLlmOutput(next) = r {
                if !next.is_empty() {
                    out = next;
                }
            }
        }
        out
    }

    fn persist_turn_session(&self, messages: &[Message]) {
        let cfg = self.config();
        let Some(ref sid) = cfg.session_id else {
            return;
        };
        if sid.trim().is_empty() {
            return;
        }
        let Some(sp) = self.session_persistence() else {
            return;
        };
        // Transcript: user/assistant/tool only — system belongs in `sessions.system_prompt`.
        let transcript = strip_system_messages_from_history(messages);
        let sys = leading_system_prompt_for_persist(messages);
        let platform = cfg.platform.as_deref();
        let model = self.active_model();
        let mut cursor = self
            .session_db_flush
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default();
        let result = sp.persist_session(
            sid,
            &transcript,
            &mut cursor,
            Some(model.as_str()),
            platform,
            None,
            sys.as_deref(),
        );
        if let Ok(mut guard) = self.session_db_flush.lock() {
            *guard = cursor;
        }
        if let Err(err) = result {
            tracing::warn!(session_id = %sid, "persist_session after run_conversation: {}", err);
        }
    }

    /// Active task id for this turn (Python `agent._current_task_id`).
    pub fn current_task_id(&self) -> Option<String> {
        self.current_task_id.lock().ok().and_then(|g| g.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_messages_strips_system_from_history() {
        let messages = vec![
            Message::system("You are helpful."),
            Message::user("old"),
            Message::assistant("hi"),
            Message::user("new"),
        ];
        let (hist, user) = split_messages_for_run_conversation(&messages).expect("split");
        assert_eq!(user, "new");
        assert_eq!(hist.len(), 2);
        assert!(hist.iter().all(|m| m.role != MessageRole::System));
    }

    #[test]
    fn split_messages_peels_last_user() {
        let messages = vec![
            Message::user("old"),
            Message::assistant("hi"),
            Message::user("new"),
        ];
        let (hist, user) = split_messages_for_run_conversation(&messages).expect("split");
        assert_eq!(user, "new");
        assert_eq!(hist.len(), 2);
    }

    #[tokio::test]
    async fn prepare_turn_appends_user_and_sets_task_id() {
        use crate::agent_loop::AgentConfig;
        use crate::agent_loop::ToolRegistry;
        use futures::StreamExt;
        use std::sync::Arc;

        struct StopProvider;
        #[async_trait::async_trait]
        impl hermes_core::LlmProvider for StopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "t".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>>
            {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(StopProvider),
        );
        let prepared = agent
            .prepare_turn(&RunConversationParams {
                user_message: "current".into(),
                conversation_history: vec![Message::user("prior")],
                task_id: Some("task-x".into()),
                stream_callback: None,
                persist_user_message: None,
                tools: None,
                persist_session: false,
            })
            .await
            .expect("prepare");
        assert_eq!(prepared.meta.task_id, "task-x");
        assert_eq!(agent.current_task_id().as_deref(), Some("task-x"));
        assert_eq!(prepared.messages.len(), 2);
    }

    #[test]
    fn hydrate_memory_nudge_from_history() {
        use crate::agent_loop::AgentConfig;
        use crate::agent_loop::ToolRegistry;
        use futures::StreamExt;
        use std::sync::Arc;

        struct StopProvider;
        #[async_trait::async_trait]
        impl hermes_core::LlmProvider for StopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "t".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>>
            {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig {
                memory_nudge_interval: 4,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(StopProvider),
        );
        let history: Vec<Message> = (0..5).map(|i| Message::user(format!("u{i}"))).collect();
        agent.hydrate_memory_nudge_counters_from_history(&history);
        let turns = agent
            .evolution_counters
            .lock()
            .expect("lock")
            .turns_since_memory;
        assert_eq!(turns, 1);
    }

    #[test]
    fn last_reasoning_stops_at_turn_boundary() {
        let mut stale = Message::assistant("old");
        stale.reasoning_content = Some("stale".into());
        let mut fresh = Message::assistant("ok");
        fresh.reasoning_content = Some("fresh".into());
        let messages = vec![
            Message::user("prior"),
            stale,
            Message::user("current"),
            fresh,
        ];
        assert_eq!(
            extract_last_reasoning_current_turn(&messages).as_deref(),
            Some("fresh")
        );
    }
}
