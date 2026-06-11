use std::time::{Duration, Instant};

use hermes_agent::{RunConversationParams, split_messages_for_run_conversation};
use hermes_core::AgentError;

use crate::model_switch::provider_model_ids;

use super::App;

impl App {
    pub(crate) async fn run_messages_with_current_agent(
        &self,
        messages: Vec<hermes_core::Message>,
        stream_enabled: bool,
    ) -> Result<hermes_core::AgentResult, AgentError> {
        self.run_messages_with_current_agent_tools(messages, stream_enabled, true)
            .await
    }

    pub(super) async fn run_messages_with_current_agent_tools(
        &self,
        messages: Vec<hermes_core::Message>,
        stream_enabled: bool,
        include_tools: bool,
    ) -> Result<hermes_core::AgentResult, AgentError> {
        let tool_schemas = include_tools.then(|| self.core.tool_schemas.clone());
        let task_id = Some(self.session.session_id.clone());
        let (history, user_message) = split_messages_for_run_conversation(&messages)
            .ok_or_else(|| AgentError::Config("no user message in turn".into()))?;
        if stream_enabled && self.core.config.streaming.enabled {
            let stream_handle = self.stream.stream_handle.clone();
            let stream_cb: Option<Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>> =
                stream_handle.map(|h| {
                    Box::new(move |chunk: hermes_core::StreamChunk| {
                        h.send_chunk(chunk);
                    }) as Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>
                });
            let conv = self
                .core
                .agent
                .run_conversation(RunConversationParams {
                    user_message,
                    conversation_history: history,
                    task_id,
                    stream_callback: stream_cb,
                    persist_user_message: None,
                    tools: tool_schemas,
                    persist_session: false,
                })
                .await?;
            Ok(conv.into_loop_result())
        } else {
            let conv = self
                .core
                .agent
                .run_conversation(RunConversationParams {
                    user_message,
                    conversation_history: history,
                    task_id,
                    stream_callback: None,
                    persist_user_message: None,
                    tools: tool_schemas,
                    persist_session: false,
                })
                .await?;
            Ok(conv.into_loop_result())
        }
    }

    /// Run the agent on the current message history.
    ///
    /// Sends all messages to the agent loop and appends the result.
    /// Checks the interrupt controller before running and clears it after.
    pub(super) async fn run_agent(&mut self) -> Result<(), AgentError> {
        let run_started_at = Instant::now();
        self.maybe_autopin_contextlattice_topic_from_objective();
        Self::emit_phase_event(
            &self.stream.stream_handle_shared,
            "preflight",
            "runtime preflight + credential hydration",
            5,
        );
        self.emit_contextlattice_connectivity_status().await;
        let provider = self.current_runtime_provider();
        let force_refresh = Self::should_force_preflight_auth_refresh(provider.as_str());
        self.refresh_runtime_provider_credentials_if_needed(force_refresh)
            .await;
        if force_refresh {
            Self::emit_lifecycle_event(
                &self.stream.stream_handle_shared,
                format!("preflight auth refresh forced for provider {}", provider),
            );
        }
        if let Some(policy) = self.quorum_mode_armed_for_turn() {
            self.runtime.quorum_armed_once = false;
            self.clear_quorum_system_hints_inplace();
            self.core.interrupt_controller.clear_interrupt();
            match self.run_quorum_fanout_turn(run_started_at, policy).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(err) => return Err(err),
            }
        }
        Self::emit_phase_event(
            &self.stream.stream_handle_shared,
            "dispatch",
            "dispatching model request",
            15,
        );
        self.core.interrupt_controller.clear_interrupt();
        let mut remediation_attempted = false;
        let mut auth_refresh_attempts = 0usize;
        let auth_refresh_retry_limit = Self::auth_refresh_retry_limit();
        let mut transient_retry_attempts = 0usize;
        let transient_retry_limit = Self::transient_retry_limit();
        let mut objective_continuation_attempts = 0usize;
        let objective_continuation_limit = Self::objective_continuation_retry_limit();
        loop {
            Self::emit_lifecycle_event(
                &self.stream.stream_handle_shared,
                format!(
                    "dispatching request to {} (messages={})",
                    self.model.current_model,
                    self.session.messages.len()
                ),
            );
            Self::emit_phase_event(
                &self.stream.stream_handle_shared,
                "inference",
                "model inference + tool execution",
                35,
            );
            let baseline_len = self.session.messages.len();
            let (messages, reformulated) = self.build_inference_messages();
            if reformulated {
                Self::emit_lifecycle_event(
                    &self.stream.stream_handle_shared,
                    "runtime prompt reformulation injected (anti-scheming + context + tool routing + contradiction self-check)",
                );
            }
            let result = self.run_messages_with_current_agent(messages, true).await;

            match result {
                Ok(result) => {
                    let total_turns = result.total_turns;
                    let interrupted = result.interrupted;
                    let finished_naturally = result.finished_naturally;
                    if objective_continuation_attempts < objective_continuation_limit {
                        if let Some(reason) =
                            self.should_force_objective_continuation(&result, baseline_len)
                        {
                            self.session.messages = result.messages;
                            self.session.messages.push(hermes_core::Message::system(
                                Self::objective_continuation_system_prompt(&reason),
                            ));
                            self.prune_ui_after_current_messages();
                            objective_continuation_attempts += 1;
                            Self::emit_lifecycle_event(
                                &self.stream.stream_handle_shared,
                                format!(
                                    "objective continuation enforcer triggered ({}/{}): {}",
                                    objective_continuation_attempts,
                                    objective_continuation_limit,
                                    reason
                                ),
                            );
                            Self::emit_phase_event(
                                &self.stream.stream_handle_shared,
                                "objective",
                                "auto-continuing objective loop for concrete execution",
                                50,
                            );
                            continue;
                        }
                    }
                    if let Err(err) = self.apply_agent_result_and_persist(result) {
                        tracing::warn!("session autosave skipped: {}", err);
                    }
                    Self::emit_lifecycle_event(
                        &self.stream.stream_handle_shared,
                        format!(
                            "run finished in {:.2}s (total_turns={})",
                            run_started_at.elapsed().as_secs_f64(),
                            total_turns
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream.stream_handle_shared,
                        "finalize",
                        "transcript finalization + persistence",
                        100,
                    );
                    if let Some(handle) = &self.stream.stream_handle {
                        handle.send_done();
                    }
                    if interrupted {
                        tracing::info!("Agent loop returned interrupted=true (graceful stop)");
                        if self.stream.stream_handle.is_some() {
                            self.push_ui_assistant("[Agent execution interrupted]");
                        } else {
                            println!("[Agent execution interrupted]");
                        }
                    } else if !finished_naturally {
                        tracing::warn!(
                            "Agent stopped after {} turns (did not finish naturally)",
                            total_turns
                        );
                    }
                    break;
                }
                Err(AgentError::Interrupted { message }) => {
                    self.core.interrupt_controller.clear_interrupt();
                    Self::emit_lifecycle_event(
                        &self.stream.stream_handle_shared,
                        format!(
                            "run interrupted after {:.2}s",
                            run_started_at.elapsed().as_secs_f64()
                        ),
                    );
                    if let Some(handle) = &self.stream.stream_handle {
                        handle.send_done();
                    }
                    if let Some(redirect) = message {
                        tracing::info!("Agent interrupted with redirect: {}", redirect);
                    } else {
                        tracing::info!("Agent interrupted by user");
                    }
                    if self.stream.stream_handle.is_some() {
                        self.push_ui_assistant("[Agent execution interrupted]");
                    } else {
                        println!("[Agent execution interrupted]");
                    }
                    break;
                }
                Err(e) => {
                    Self::emit_lifecycle_event(
                        &self.stream.stream_handle_shared,
                        format!(
                            "run error after {:.2}s: {}",
                            run_started_at.elapsed().as_secs_f64(),
                            e
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream.stream_handle_shared,
                        "recovery",
                        "error handling + remediation",
                        60,
                    );
                    if let Some(handle) = &self.stream.stream_handle {
                        handle.send_done();
                    }
                    if Self::is_provider_auth_or_session_error(&e) {
                        if auth_refresh_attempts < auth_refresh_retry_limit {
                            if self.force_auth_refresh_after_error().await {
                                auth_refresh_attempts += 1;
                                Self::emit_lifecycle_event(
                                    &self.stream.stream_handle_shared,
                                    format!(
                                        "auth refresh retry {}/{}",
                                        auth_refresh_attempts, auth_refresh_retry_limit
                                    ),
                                );
                                continue;
                            }
                        } else {
                            Self::emit_lifecycle_event(
                                &self.stream.stream_handle_shared,
                                format!(
                                    "auth refresh retries exhausted ({})",
                                    auth_refresh_retry_limit
                                ),
                            );
                        }
                    }
                    if Self::is_transient_retryable_error(&e)
                        && transient_retry_attempts < transient_retry_limit
                    {
                        transient_retry_attempts += 1;
                        let backoff_ms = (transient_retry_attempts as u64)
                            .saturating_mul(1_000)
                            .max(800);
                        Self::emit_lifecycle_event(
                            &self.stream.stream_handle_shared,
                            format!(
                                "transient runtime error retry {}/{} after {}ms: {}",
                                transient_retry_attempts, transient_retry_limit, backoff_ms, e
                            ),
                        );
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    if !remediation_attempted {
                        if let Some((next_model, notice)) =
                            self.model_auto_remediation_target(&e).await
                        {
                            tracing::warn!(
                                "Model auto-remediation triggered: {} -> {}",
                                self.model.current_model,
                                next_model
                            );
                            if self.stream.stream_handle.is_some() {
                                self.push_ui_assistant(notice.clone());
                            } else {
                                println!("{notice}");
                            }
                            Self::emit_lifecycle_event(
                                &self.stream.stream_handle_shared,
                                format!("auto-remediation switching model to {}", next_model),
                            );
                            self.switch_model(&next_model);
                            remediation_attempted = true;
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    pub(super) fn model_auto_remediation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_MODEL_AUTO_REMEDIATE")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    pub(super) fn is_model_not_found_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let model_not_found = message.contains("model not found")
            || message.contains("requested model does not exist")
            || message.contains("404 not found")
            || message.contains("openrouter catalog");
        model_not_found && message.contains("model")
    }

    pub(super) fn is_provider_auth_or_session_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        message.contains("401")
            || message.contains("403")
            || message.contains("unauthorized")
            || message.contains("invalid token")
            || message.contains("token_expired")
            || message.contains("expired_token")
            || message.contains("token expired")
            || message.contains("invalid_token")
            || message.contains("expired")
            || message.contains("authentication")
            || message.contains("session expired")
    }

    pub(super) fn is_provider_tool_payload_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let mentions_tool_payload =
            message.contains("tool") || message.contains("function") || message.contains("schema");
        let provider_payload_rejected = message.contains("provider returned error")
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("check the model name")
                || message.contains("invalid"));
        let openai_shape_rejected = (message.contains("no choices in response")
            || message.contains("empty choices array"))
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("provider returned error")
                || message.contains("invalid"));
        let explicit_tool_schema_rejected =
            message.contains("tool") && (message.contains("invalid") || message.contains("schema"));
        let strict_function_shape =
            message.contains("invalid input") && message.contains("function");
        provider_payload_rejected
            || openai_shape_rejected
            || explicit_tool_schema_rejected
            || strict_function_shape
            || (message.contains("422") && message.contains("valid payload"))
    }

    pub(super) async fn model_auto_remediation_target(
        &self,
        err: &AgentError,
    ) -> Option<(String, String)> {
        if !Self::model_auto_remediation_enabled() || !Self::is_model_not_found_error(err) {
            return None;
        }

        let (provider, current_model_id) = self
            .model
            .current_model
            .split_once(':')
            .unwrap_or(("openai", self.model.current_model.as_str()));
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            return None;
        }

        let catalog = provider_model_ids(&provider).await;
        if catalog.is_empty() {
            return None;
        }

        let selected = Self::resolve_quorum_catalog_candidate(current_model_id, &catalog)
            .or_else(|| catalog.first().cloned())?;

        let next_model = format!("{}:{}", provider, selected.trim());
        if next_model.eq_ignore_ascii_case(&self.model.current_model) {
            return None;
        }
        let close = Self::rank_catalog_candidates(current_model_id, &catalog, 3);
        let notice = format!(
            "Model catalog remediation: `{}` failed with not-found; switching to `{}` and retrying once. close matches: {}",
            self.model.current_model,
            next_model,
            if close.is_empty() {
                "(none)".to_string()
            } else {
                close.join(", ")
            }
        );
        Some((next_model, notice))
    }
}
