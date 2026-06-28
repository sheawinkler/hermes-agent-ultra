impl App {
    fn push_stream_extra_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        payload: serde_json::Value,
    ) {
        if let Ok(guard) = shared.lock() {
            if let Some(handle) = guard.clone() {
                handle.send_chunk(hermes_core::StreamChunk {
                    delta: Some(hermes_core::StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(payload),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
        }
    }

    fn preview_for_status(raw: &str, max_chars: usize) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() <= max_chars {
            collapsed
        } else {
            let mut out: String = collapsed
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect();
            out.push('…');
            out
        }
    }

    fn set_env_if_changed(key: &str, value: &str) -> bool {
        let next = value.trim();
        if next.is_empty() {
            return false;
        }
        let current = std::env::var(key).ok().unwrap_or_default();
        if current == next {
            return false;
        }
        std::env::set_var(key, next);
        true
    }

    fn bool_env(key: &str) -> Option<bool> {
        let raw = std::env::var(key).ok()?;
        let normalized = raw.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }

    fn is_unbounded_token(raw: &str) -> bool {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "off" | "unlimited" | "infinite" | "max"
        )
    }

    fn auth_refresh_retry_limit() -> usize {
        std::env::var("HERMES_AUTH_REFRESH_MAX_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(3)
    }

    fn quorum_voter_retry_limit() -> usize {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_VOTER_MAX_RETRIES") {
            if Self::is_unbounded_token(&raw) {
                return 16;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return parsed.max(2);
            }
        }
        Self::auth_refresh_retry_limit().max(6)
    }

    fn transient_retry_limit() -> usize {
        std::env::var("HERMES_TRANSIENT_MAX_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(2)
    }

    fn is_transient_retryable_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg)
            | AgentError::Io(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        message.contains("timed out")
            || message.contains("timeout")
            || message.contains("connection reset")
            || message.contains("connection refused")
            || message.contains("temporarily unavailable")
            || message.contains("try again")
            || message.contains("rate limit")
            || message.contains("429")
            || message.contains("502")
            || message.contains("503")
            || message.contains("504")
            || message.contains("provider rejected")
    }

    fn objective_execution_enforcer_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_OBJECTIVE_EXECUTION_ENFORCER")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn objective_continuation_retry_limit() -> usize {
        std::env::var("HERMES_OBJECTIVE_CONTINUATION_MAX_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(1)
    }

    fn load_active_objective_contract() -> Option<ObjectiveContract> {
        load_objective_contract()
            .ok()
            .flatten()
            .filter(|contract| objective_lifecycle_is_active(&contract.lifecycle_status))
    }

    fn looks_like_status_only_output(text: &str) -> bool {
        let lowered = text.trim().to_ascii_lowercase();
        if lowered.is_empty() {
            return true;
        }

        let has_future_language = [
            "i will",
            "i'll",
            "next i",
            "going to",
            "plan:",
            "i can",
            "we should",
            "i would",
            "i'll proceed",
            "i will proceed",
            "proceeding with",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));
        let has_execution_evidence = [
            "path=",
            "file=",
            "exit code",
            "result:",
            "tested",
            "verified",
            "implemented",
            "changed",
            "patched",
            "command:",
            "run_id",
            "metric",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));

        let has_weakness_markers = [
            "let me know",
            "if you'd like",
            "i can do that next",
            "awaiting",
            "need your confirmation",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));

        (has_future_language && !has_execution_evidence) || has_weakness_markers
    }

    #[cfg(unix)]
    fn objective_pid_is_alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs a liveness/permission probe without sending a signal.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            true
        } else {
            matches!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            )
        }
    }

    #[cfg(not(unix))]
    fn objective_pid_is_alive(_pid: u32) -> bool {
        false
    }

    fn clear_stale_objective_wait_barrier(&self, reason: &str) {
        match clear_objective_contract_wait_barrier() {
            Ok(updated) => Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "objective wait barrier auto-cleared: {} (objective_id={})",
                    reason, updated.id
                ),
            ),
            Err(err) => tracing::warn!("objective wait barrier auto-clear failed: {}", err),
        }
    }

    async fn process_session_is_running(&self, session_id: &str) -> bool {
        let raw = self
            .tool_registry
            .dispatch_async("process", json!({"action":"poll","session_id":session_id}))
            .await;
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return false;
        };
        value
            .get("status")
            .and_then(Value::as_str)
            .map(|status| status.eq_ignore_ascii_case("running"))
            .unwrap_or(false)
    }

    async fn objective_wait_barrier_active_message(
        &self,
        contract: &ObjectiveContract,
    ) -> Option<String> {
        match objective_wait_target(contract)? {
            ObjectiveWaitTarget::Pid(pid) => {
                if Self::objective_pid_is_alive(pid) {
                    Some(format!(
                        "objective wait barrier active: {}",
                        summarize_objective_wait_barrier(contract)
                    ))
                } else {
                    self.clear_stale_objective_wait_barrier(&format!("pid {pid} is not running"));
                    None
                }
            }
            ObjectiveWaitTarget::Session(session_id) => {
                if self.process_session_is_running(&session_id).await {
                    Some(format!(
                        "objective wait barrier active: {}",
                        summarize_objective_wait_barrier(contract)
                    ))
                } else {
                    self.clear_stale_objective_wait_barrier(&format!(
                        "process session {session_id} is not running"
                    ));
                    None
                }
            }
            ObjectiveWaitTarget::Time { until_unix_ms } => {
                if objective_now_unix_ms() < until_unix_ms {
                    let remaining = objective_wait_remaining_seconds(contract).unwrap_or_default();
                    Some(format!(
                        "objective wait barrier active: {} (remaining_seconds={})",
                        summarize_objective_wait_barrier(contract),
                        remaining.max(0)
                    ))
                } else {
                    self.clear_stale_objective_wait_barrier("timer elapsed");
                    None
                }
            }
        }
    }

    async fn should_force_objective_continuation(
        &self,
        result: &hermes_core::AgentResult,
        baseline_len: usize,
    ) -> Option<String> {
        if !Self::objective_execution_enforcer_enabled() {
            return None;
        }
        let contract = Self::load_active_objective_contract()?;
        let behavior_mode = canonical_objective_behavior_mode(&contract.behavior_mode);
        if !matches!(behavior_mode.as_str(), "autonomous" | "mission") {
            return None;
        }
        if let Some(message) = self.objective_wait_barrier_active_message(&contract).await {
            Self::emit_lifecycle_event(&self.stream_handle_shared, message);
            return None;
        }

        let new_messages = if result.messages.len() > baseline_len {
            &result.messages[baseline_len..]
        } else {
            &result.messages[..]
        };

        let had_tool_activity = new_messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::Tool
                || (message.role == hermes_core::MessageRole::Assistant
                    && message
                        .tool_calls
                        .as_ref()
                        .map(|calls| !calls.is_empty())
                        .unwrap_or(false))
        });
        if had_tool_activity {
            return None;
        }

        let output = Self::extract_last_assistant_output(new_messages);
        if output.trim().is_empty() {
            return Some(
                "assistant returned empty output while objective remained active".to_string(),
            );
        }
        if Self::looks_like_status_only_output(&output) {
            return Some(
                "assistant output was status/plan-heavy without concrete executed action"
                    .to_string(),
            );
        }
        None
    }

    fn objective_continuation_system_prompt(reason: &str) -> String {
        format!(
            "[OBJECTIVE_CONTINUATION_ENFORCER]\n\
             reason={}\n\
             Continue objective execution immediately.\n\
             Requirements for this pass:\n\
             1) execute at least one concrete action (tool or code operation),\n\
             2) include verifiable evidence from that action,\n\
             3) report objective delta in measurable terms,\n\
             4) end with the next highest-value action and continue momentum.\n\
             Do not return a plan-only or defer-only response.",
            reason
        )
    }

}
