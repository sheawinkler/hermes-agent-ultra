impl App {
    const SESSION_OBJECTIVE_PREFIX: &'static str = "[SESSION_OBJECTIVE] ";

    fn ensure_session_stub_snapshot(&self) {
        if let Err(err) = self.persist_session_snapshot(None) {
            tracing::warn!("session startup snapshot skipped: {}", err);
        }
    }

    fn snapshot_file_is_empty_session(path: &Path, session_id: &str) -> bool {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return false;
        };
        let Some(snapshot_session_id) = value
            .get("session_info")
            .and_then(|info| info.get("session_id"))
            .and_then(|value| value.as_str())
        else {
            return false;
        };
        if snapshot_session_id != session_id {
            return false;
        }
        value
            .get("messages")
            .and_then(|messages| messages.as_array())
            .is_some_and(|messages| messages.is_empty())
    }

    fn remove_empty_snapshot_file(&self, session_id: &str) -> Result<bool, AgentError> {
        let snapshot_path = self
            .state_root
            .join("sessions")
            .join(format!("{session_id}.json"));
        if !Self::snapshot_file_is_empty_session(&snapshot_path, session_id) {
            return Ok(false);
        }
        std::fs::remove_file(&snapshot_path).map_err(|e| {
            AgentError::Io(format!(
                "Failed to remove empty session snapshot {}: {}",
                snapshot_path.display(),
                e
            ))
        })?;
        Ok(true)
    }

    fn discard_session_if_empty(
        &self,
        session_id: &str,
        message_count: usize,
        has_session_objective: bool,
    ) -> bool {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return false;
        }

        let mut discarded = false;
        match SessionPersistence::new(&self.state_root).delete_session_if_empty(session_id) {
            Ok(deleted) => discarded |= deleted,
            Err(err) => tracing::debug!(
                session_id,
                error = %err,
                "failed to delete empty session db row"
            ),
        }

        if message_count == 0 && !has_session_objective {
            match self.remove_empty_snapshot_file(session_id) {
                Ok(removed) => discarded |= removed,
                Err(err) => tracing::debug!(
                    session_id,
                    error = %err,
                    "failed to remove empty session snapshot"
                ),
            }
        }

        discarded
    }

    pub fn discard_current_session_if_empty(&self) -> bool {
        self.discard_session_if_empty(
            &self.session_id,
            self.messages.len(),
            self.session_objective.is_some(),
        )
    }

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

    fn should_force_preflight_auth_refresh(provider: &str) -> bool {
        if let Some(explicit) = Self::bool_env("HERMES_FORCE_RUNTIME_AUTH_REFRESH") {
            return explicit;
        }
        matches!(
            provider,
            "nous" | "qwen-oauth" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth"
        )
    }

    fn quorum_force_refresh_each_voter() -> bool {
        Self::bool_env("HERMES_QUORUM_FORCE_REFRESH_EACH_VOTER").unwrap_or(false)
    }

    fn quorum_toolless_provider_fallback_enabled() -> bool {
        !matches!(
            Self::bool_env("HERMES_QUORUM_TOOLLESS_PROVIDER_FALLBACK"),
            Some(false)
        )
    }

    fn quorum_voter_tools_enabled() -> bool {
        !matches!(Self::bool_env("HERMES_QUORUM_VOTER_TOOLS"), Some(false))
    }

    fn quorum_synthesis_tools_enabled() -> bool {
        !matches!(Self::bool_env("HERMES_QUORUM_SYNTHESIS_TOOLS"), Some(false))
    }

    fn nous_refresh_contention_error(err: &AgentError) -> bool {
        let text = err.to_string().to_ascii_lowercase();
        text.contains("slow_down")
            || text.contains("too many requests")
            || text.contains("refresh already in progress")
            || text.contains("429")
    }

    fn apply_nous_runtime_credentials(creds: &NousRuntimeCredentials) -> bool {
        let mut changed = false;
        changed |= Self::set_env_if_changed("NOUS_API_KEY", &creds.api_key);
        if !creds.base_url.trim().is_empty() {
            changed |= Self::set_env_if_changed("NOUS_INFERENCE_BASE_URL", &creds.base_url);
        }
        changed
    }

    fn contextlattice_ui_status_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_CONTEXTLATTICE_UI_STATUS")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn contextlattice_orchestrator_url() -> String {
        std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
            .ok()
            .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:8075".to_string())
    }

    fn contextlattice_ping_timeout_secs() -> u64 {
        std::env::var("HERMES_CONTEXTLATTICE_PING_TIMEOUT_SECONDS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(12)
            .clamp(1, 120)
    }

    async fn emit_contextlattice_connectivity_status(&self) {
        if !Self::contextlattice_ui_status_enabled() {
            return;
        }
        let base = Self::contextlattice_orchestrator_url();
        let url = format!("{}/status", base);
        let topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "runbooks/hermes".to_string());
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("contextlattice preflight ping {} (topic={})", base, topic),
        );
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(Self::contextlattice_ping_timeout_secs()))
            .build()
        {
            Ok(c) => c,
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("contextlattice client init failed: {}", err),
                );
                return;
            }
        };
        match client.get(&url).send().await {
            Ok(resp) => {
                let status_code = resp.status();
                if status_code.is_success() {
                    let parsed = resp.json::<serde_json::Value>().await.ok();
                    let service = parsed
                        .as_ref()
                        .and_then(|v| v.get("service").and_then(|s| s.as_str()))
                        .unwrap_or("unknown");
                    let ok_flag = parsed
                        .as_ref()
                        .and_then(|v| v.get("ok").and_then(|s| s.as_bool()))
                        .unwrap_or(true);
                    let detail = if ok_flag { "connected" } else { "degraded" };
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "contextlattice {} (service={} status={} endpoint={})",
                            detail, service, status_code, base
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream_handle_shared,
                        "context",
                        if ok_flag {
                            "contextlattice connected"
                        } else {
                            "contextlattice degraded"
                        },
                        12,
                    );
                } else {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "contextlattice status endpoint returned {} ({})",
                            status_code, url
                        ),
                    );
                }
            }
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("contextlattice preflight failed: {} ({})", err, url),
                );
            }
        }
    }

    fn auto_nous_reauth_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_AUTO_NOUS_REAUTH")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn auth_error_requires_nous_login(err: &AgentError) -> bool {
        let text = err.to_string().to_ascii_lowercase();
        text.contains("not logged into nous portal")
            || text.contains("run `hermes portal`")
            || text.contains("re-run `hermes auth nous`")
            || text.contains("stored nous auth state is invalid")
            || text.contains("missing refresh token")
            || text.contains("invalid nous refresh response")
    }

    async fn attempt_interactive_nous_login(&mut self, reason: &str) -> bool {
        if !Self::auto_nous_reauth_enabled() {
            return false;
        }
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("Nous OAuth re-auth required ({reason}); launching portal login flow"),
        );
        match login_nous_device_code(NousDeviceCodeOptions::default()).await {
            Ok(state) => match save_nous_auth_state(&state) {
                Ok(path) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!("Nous OAuth state refreshed: {}", path.display()),
                    );
                    true
                }
                Err(err) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!("Nous OAuth state save failed: {}", err),
                    );
                    false
                }
            },
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("Nous OAuth interactive login failed: {}", err),
                );
                false
            }
        }
    }

    async fn refresh_runtime_provider_credentials_if_needed(&mut self, force_refresh: bool) {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        let provider = normalize_runtime_provider_name(provider_name.as_str());
        let mut rotated = false;
        let mut note: Option<String> = None;

        match provider.as_str() {
            "nous" => match resolve_nous_runtime_credentials(
                force_refresh,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    rotated |= Self::apply_nous_runtime_credentials(&creds);
                    if rotated {
                        note = Some("refreshed Nous runtime credential".to_string());
                    }
                }
                Err(e) => {
                    if force_refresh && Self::nous_refresh_contention_error(&e) {
                        match resolve_nous_runtime_credentials(
                            false,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                rotated |= Self::apply_nous_runtime_credentials(&creds);
                                note = Some(
                                    "Nous refresh busy; reused cached runtime credential"
                                        .to_string(),
                                );
                            }
                            Err(cache_err) => {
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "warning: Nous cached credential hydration failed after refresh contention ({cache_err})"
                                    ),
                                );
                            }
                        }
                    }
                    if Self::auth_error_requires_nous_login(&e)
                        && self
                            .attempt_interactive_nous_login("credential missing or invalid")
                            .await
                    {
                        match resolve_nous_runtime_credentials(
                            true,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                rotated |= Self::apply_nous_runtime_credentials(&creds);
                                if rotated {
                                    note = Some("refreshed Nous runtime credential".to_string());
                                }
                            }
                            Err(err) => {
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!("warning: Nous credential refresh skipped ({err})"),
                                );
                            }
                        }
                    } else {
                        if !rotated && note.is_none() {
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!("warning: Nous credential refresh skipped ({e})"),
                            );
                        }
                    }
                }
            },
            "qwen-oauth" => match resolve_qwen_runtime_credentials(
                force_refresh,
                true,
                QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    rotated |=
                        Self::set_env_if_changed("HERMES_QWEN_OAUTH_API_KEY", &creds.api_key);
                    rotated |= Self::set_env_if_changed("DASHSCOPE_API_KEY", &creds.api_key);
                    if !creds.base_url.trim().is_empty() {
                        rotated |=
                            Self::set_env_if_changed("HERMES_QWEN_BASE_URL", &creds.base_url);
                    }
                    if rotated {
                        note = Some("refreshed Qwen OAuth runtime credential".to_string());
                    }
                }
                Err(e) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!("warning: Qwen OAuth refresh skipped ({e})"),
                    );
                }
            },
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                match resolve_gemini_oauth_runtime_credentials(force_refresh).await {
                    Ok(creds) => {
                        rotated |=
                            Self::set_env_if_changed("HERMES_GEMINI_OAUTH_API_KEY", &creds.api_key);
                        rotated |= Self::set_env_if_changed("GOOGLE_API_KEY", &creds.api_key);
                        rotated |= Self::set_env_if_changed("GEMINI_API_KEY", &creds.api_key);
                        if rotated {
                            note = Some("refreshed Gemini OAuth runtime credential".to_string());
                        }
                    }
                    Err(e) => {
                        Self::emit_lifecycle_event(
                            &self.stream_handle_shared,
                            format!("warning: Gemini OAuth refresh skipped ({e})"),
                        );
                    }
                }
            }
            _ => {}
        }

        if rotated {
            self.switch_model(&self.current_model.clone());
        }
        if let Some(msg) = note {
            Self::emit_lifecycle_event(&self.stream_handle_shared, msg);
        }
    }

    fn stream_callbacks(shared: Arc<StdMutex<Option<StreamHandle>>>) -> AgentCallbacks {
        let thinking_shared = shared.clone();
        let tool_start_shared = shared.clone();
        let tool_done_shared = shared.clone();
        let status_shared = shared;
        AgentCallbacks {
            on_thinking: Some(Box::new(move |thinking: &str| {
                let preview = App::preview_for_status(thinking, 220);
                if preview.is_empty() {
                    return;
                }
                App::push_stream_extra_event(
                    &thinking_shared,
                    serde_json::json!({
                        "ui_event": "thinking",
                        "text": preview,
                    }),
                );
            })),
            on_tool_start: Some(Box::new(move |tool: &str, args: &Value| {
                let arg_preview = App::preview_for_status(&args.to_string(), 140);
                App::push_stream_extra_event(
                    &tool_start_shared,
                    serde_json::json!({
                        "ui_event": "tool_start",
                        "tool": tool,
                        "args_preview": arg_preview,
                    }),
                );
            })),
            on_tool_complete: Some(Box::new(move |tool: &str, content: &str| {
                let preview = App::preview_for_status(content, 160);
                App::push_stream_extra_event(
                    &tool_done_shared,
                    serde_json::json!({
                        "ui_event": "tool_complete",
                        "tool": tool,
                        "result_preview": preview,
                    }),
                );
            })),
            status_callback: Some(Arc::new(move |event_type: &str, message: &str| {
                let preview = App::preview_for_status(message, 200);
                if preview.is_empty() {
                    return;
                }
                App::push_stream_extra_event(
                    &status_shared,
                    serde_json::json!({
                        "ui_event": "status",
                        "event_type": event_type,
                        "message": preview,
                    }),
                );
            })),
            ..AgentCallbacks::default()
        }
    }

    fn emit_lifecycle_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        message: impl AsRef<str>,
    ) {
        let preview = App::preview_for_status(message.as_ref(), 220);
        if preview.is_empty() {
            return;
        }
        if App::oneshot_lifecycle_stdout_enabled(shared) {
            println!("[lifecycle] {}", preview);
        }
        App::push_stream_extra_event(
            shared,
            serde_json::json!({
                "ui_event": "lifecycle",
                "message": preview,
            }),
        );
    }

    fn emit_phase_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        phase: &str,
        label: &str,
        progress_pct: u8,
    ) {
        let phase = phase.trim();
        let label = App::preview_for_status(label, 220);
        if phase.is_empty() || label.is_empty() {
            return;
        }
        if App::oneshot_lifecycle_stdout_enabled(shared) {
            println!("[phase {:>3}%] {}: {}", progress_pct.min(100), phase, label);
        }
        App::push_stream_extra_event(
            shared,
            serde_json::json!({
                "ui_event": "phase",
                "phase": phase,
                "label": label,
                "progress_pct": progress_pct.min(100),
            }),
        );
    }

    fn oneshot_lifecycle_stdout_enabled(shared: &Arc<StdMutex<Option<StreamHandle>>>) -> bool {
        let stream_attached = shared
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|_| ()))
            .is_some();
        if stream_attached {
            return false;
        }
        matches!(
            std::env::var("HERMES_ONESHOT_LIFECYCLE_STDOUT")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on")
        )
    }

    fn objective_context_autopin_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn sanitize_topic_path_segment(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/') {
                out.push(ch);
            } else {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    }

    fn maybe_autopin_contextlattice_topic_from_objective(&self) {
        if !Self::objective_context_autopin_enabled() {
            return;
        }
        let Ok(Some(contract)) = load_objective_contract() else {
            return;
        };
        let objective_id = Self::sanitize_topic_path_segment(contract.id.trim());
        if objective_id.is_empty() {
            return;
        }
        let target_topic = format!("runbooks/objective/{}", objective_id);
        let current_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let should_override = match current_topic.as_deref() {
            None => true,
            Some("runbooks/hermes") => true,
            Some(existing)
                if existing.eq_ignore_ascii_case(target_topic.as_str())
                    || !existing
                        .to_ascii_lowercase()
                        .starts_with("runbooks/objective/") =>
            {
                false
            }
            Some(_) => true,
        };
        if should_override {
            std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", &target_topic);
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "ContextLattice objective autopin set topic_path={} (objective_id={})",
                    target_topic, contract.id
                ),
            );
            Self::emit_phase_event(
                &self.stream_handle_shared,
                "context",
                "objective context autopin",
                8,
            );
        }
    }

    fn runtime_reformulation_objective(
        contract: &ObjectiveContract,
    ) -> RuntimeReformulationObjective {
        RuntimeReformulationObjective {
            id: contract.id.clone(),
            behavior_mode: canonical_objective_behavior_mode(&contract.behavior_mode),
            objective_text: contract.objective_text.clone(),
            behavior_directives: contract.behavior_directives.clone(),
            success_criteria: contract.success_criteria.clone(),
        }
    }

    fn build_runtime_reformulation_message(&self, latest_user_prompt: &str) -> Option<String> {
        let objective = Self::load_active_objective_contract()
            .as_ref()
            .map(Self::runtime_reformulation_objective);
        build_runtime_reformulation_message_for_runtime(latest_user_prompt, objective.as_ref())
    }

    fn build_inference_messages(&self) -> (Vec<hermes_core::Message>, bool) {
        let mut messages = self.messages.clone();
        let Some(last_user_idx) = messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User)
        else {
            return (messages, false);
        };
        let user_prompt = messages[last_user_idx]
            .content
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        let Some(reformulation) = self.build_runtime_reformulation_message(&user_prompt) else {
            return (messages, false);
        };
        messages.insert(last_user_idx, hermes_core::Message::system(reformulation));
        (messages, true)
    }

    fn compose_quorum_messages(
        control_sections: Vec<String>,
        base_messages: Vec<hermes_core::Message>,
        trailing_user_context: Option<String>,
    ) -> Vec<hermes_core::Message> {
        let control_context = control_sections
            .into_iter()
            .map(|section| section.trim().to_string())
            .filter(|section| !section.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut merged_system_sections: Vec<String> = Vec::new();
        let mut non_system_messages: Vec<hermes_core::Message> = Vec::new();

        for message in base_messages {
            if message.role == hermes_core::MessageRole::System {
                if let Some(content) = message.content.as_deref().map(str::trim) {
                    if !content.is_empty() {
                        merged_system_sections.push(content.to_string());
                    }
                }
            } else {
                non_system_messages.push(message);
            }
        }

        let mut messages = Vec::new();
        if !merged_system_sections.is_empty() {
            messages.push(hermes_core::Message::system(
                merged_system_sections.join("\n\n"),
            ));
        }
        if !control_context.is_empty() {
            messages.push(hermes_core::Message::user(format!(
                "[QUORUM_CONTROL]\n{}",
                control_context
            )));
        }
        messages.extend(non_system_messages);
        if let Some(context) = trailing_user_context
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            messages.push(hermes_core::Message::user(context));
        }
        messages
    }

    fn moa_provider_is_virtual(provider: &str) -> bool {
        normalize_runtime_provider_name(provider) == MOA_PROVIDER
    }

    fn moa_preset_name_for_model(provider_model: &str) -> Option<String> {
        let trimmed = provider_model.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (provider, preset) = trimmed
            .split_once(':')
            .map(|(provider, preset)| (provider.trim(), preset.trim()))
            .unwrap_or((trimmed, MOA_DEFAULT_PRESET));
        if !Self::moa_provider_is_virtual(provider) {
            return None;
        }
        let preset = if preset.is_empty() {
            MOA_DEFAULT_PRESET
        } else {
            preset
        };
        Some(preset.to_ascii_lowercase())
    }

    fn moa_virtual_model_name(provider_model: &str) -> Option<String> {
        let preset = Self::moa_preset_name_for_model(provider_model)?;
        let canonical = format!("{MOA_PROVIDER}:{preset}");
        Self::moa_runtime_preset_for_model(&canonical)?;
        Some(canonical)
    }

    fn moa_runtime_preset_for_model(provider_model: &str) -> Option<MoaRuntimePreset> {
        match Self::moa_preset_name_for_model(provider_model)?.as_str() {
            MOA_DEFAULT_PRESET => Some(MoaRuntimePreset {
                name: MOA_DEFAULT_PRESET,
                reference_models: MOA_DEFAULT_REFERENCE_MODELS,
                aggregator_model: MOA_DEFAULT_AGGREGATOR_MODEL,
                voters: MOA_DEFAULT_REFERENCE_MODELS.len(),
                mode: "moa",
            }),
            _ => None,
        }
    }

    fn moa_quorum_policy_for_current_model(&self) -> Option<QuorumPolicy> {
        let preset = Self::moa_runtime_preset_for_model(&self.current_model)?;
        Some(QuorumPolicy {
            enabled: true,
            voters: preset.voters,
            models: preset
                .reference_models
                .iter()
                .map(|model| (*model).to_string())
                .collect(),
            mode: format!("{}-{}", preset.mode, preset.name),
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    fn quorum_synthesis_model_for_original(original_model: &str) -> String {
        Self::moa_runtime_preset_for_model(original_model)
            .map(|preset| preset.aggregator_model.to_string())
            .unwrap_or_else(|| original_model.trim().to_string())
    }

    fn quorum_mode_armed_for_turn(&self) -> Option<QuorumPolicy> {
        let has_user_turn = self
            .messages
            .iter()
            .any(|m| m.role == hermes_core::MessageRole::User);
        if let Some(policy) = self.moa_quorum_policy_for_current_model() {
            if !has_user_turn {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "moa virtual model selected but no user turn present yet; waiting for next user prompt",
                );
                return None;
            }
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "moa virtual model {} routes this turn through {} reference voters",
                    self.current_model, policy.voters
                ),
            );
            return Some(policy);
        }

        let policy = match load_quorum_policy() {
            Ok(policy) => policy,
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!("quorum policy load failed: {}", err),
                );
                return None;
            }
        };
        if !policy.enabled {
            if self.quorum_armed_once {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "quorum run requested but policy is disabled; run `/quorum on` first",
                );
            }
            return None;
        }
        let has_hint = self.messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::System
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(QUORUM_HINT_PREFIX)
        });
        if !has_user_turn {
            if self.quorum_armed_once || has_hint {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "quorum armed but no user turn present yet; waiting for next user prompt",
                );
            }
            return None;
        }
        if !(self.quorum_armed_once || has_hint) {
            let auto_arm = std::env::var("HERMES_QUORUM_AUTO_ARM")
                .ok()
                .map(|raw| {
                    matches!(
                        raw.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on" | "auto"
                    )
                })
                .unwrap_or(false);
            if auto_arm {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "quorum auto-arm enabled via HERMES_QUORUM_AUTO_ARM=1",
                );
                return Some(policy);
            }
            return None;
        }
        Some(policy)
    }

    fn clear_quorum_system_hints_inplace(&mut self) {
        self.messages.retain(|message| {
            if message.role != hermes_core::MessageRole::System {
                return true;
            }
            !message
                .content
                .as_deref()
                .unwrap_or_default()
                .starts_with(QUORUM_HINT_PREFIX)
        });
    }

    fn collect_quorum_models(policy: &QuorumPolicy, current_model: &str) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        let push_unique = |target: &mut Vec<String>, raw: &str| {
            let candidate = raw.trim();
            if candidate.is_empty() {
                return;
            }
            if target.iter().any(|existing| existing == candidate) {
                return;
            }
            target.push(candidate.to_string());
        };
        for model in &policy.models {
            push_unique(&mut models, model);
        }
        if models.is_empty() {
            push_unique(&mut models, current_model);
        }
        let max_voters = policy.voters.clamp(2, 8);
        if models.len() < max_voters {
            push_unique(&mut models, current_model);
        }
        if models.len() > max_voters {
            models.truncate(max_voters);
        }
        models
    }

    fn quorum_voter_passes() -> usize {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_VOTER_PASSES") {
            if Self::is_unbounded_token(&raw) {
                return 16;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return parsed.clamp(1, 16);
            }
        }
        QUORUM_DEFAULT_VOTER_PASSES
    }

    fn normalize_quorum_model_target(current_model: &str, raw: &str) -> String {
        let candidate = raw.trim();
        if candidate.is_empty() {
            return current_model.trim().to_string();
        }
        if let Some((provider, model)) = candidate.split_once(':') {
            return format!("{}:{}", provider.trim().to_ascii_lowercase(), model.trim());
        }
        let (provider, _) = resolve_provider_and_model(&GatewayConfig::default(), current_model);
        format!("{}:{}", provider.trim().to_ascii_lowercase(), candidate)
    }

    fn split_provider_model(provider_model: &str) -> (&str, &str) {
        if let Some((provider, model)) = provider_model.split_once(':') {
            (provider, model)
        } else {
            ("", provider_model)
        }
    }

    fn looks_like_version_pinned_model(model_id: &str) -> bool {
        let tail = model_id
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or(model_id)
            .to_ascii_lowercase();
        tail.as_bytes()
            .windows(8)
            .any(|window| window.iter().all(|byte| byte.is_ascii_digit()))
    }

    fn resolve_quorum_catalog_candidate(
        requested_model: &str,
        catalog: &[String],
    ) -> Option<String> {
        if catalog.is_empty() {
            return None;
        }
        let requested_trimmed = requested_model.trim();
        if requested_trimmed.is_empty() {
            return catalog.first().cloned();
        }
        if let Some(hit) = catalog
            .iter()
            .find(|m| m.trim().eq_ignore_ascii_case(requested_trimmed))
        {
            return Some(hit.clone());
        }
        let requested_lc = requested_trimmed.to_ascii_lowercase();
        let slash_suffix = format!("/{}", requested_lc);
        if let Some(hit) = catalog.iter().find(|m| {
            let lower = m.trim().to_ascii_lowercase();
            lower.ends_with(&slash_suffix) || lower == requested_lc
        }) {
            return Some(hit.clone());
        }
        if Self::looks_like_version_pinned_model(requested_trimmed) {
            return None;
        }
        Self::rank_catalog_candidates(requested_trimmed, catalog, 1)
            .into_iter()
            .next()
    }

    fn rank_catalog_candidates(
        requested_model: &str,
        catalog: &[String],
        limit: usize,
    ) -> Vec<String> {
        if catalog.is_empty() || limit == 0 {
            return Vec::new();
        }
        let requested = requested_model.trim().to_ascii_lowercase();
        if requested.is_empty() {
            return catalog.iter().take(limit).cloned().collect();
        }
        let requested_tail = requested.rsplit('/').next().unwrap_or(requested.as_str());
        let requested_norm: String = requested
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();

        let mut scored: Vec<(usize, usize, String)> = catalog
            .iter()
            .enumerate()
            .filter_map(|(idx, candidate)| {
                let cand_trimmed = candidate.trim();
                if cand_trimmed.is_empty() {
                    return None;
                }
                let cand = cand_trimmed.to_ascii_lowercase();
                let cand_tail = cand.rsplit('/').next().unwrap_or(cand.as_str());
                let cand_norm: String =
                    cand.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

                let mut score = 0usize;
                if cand == requested {
                    score += 10_000;
                }
                if cand_tail == requested_tail {
                    score += 8_000;
                }
                if cand.ends_with(&format!("/{}", requested_tail)) {
                    score += 6_000;
                }
                if cand.contains(requested_tail) || requested_tail.contains(cand_tail) {
                    score += 2_000;
                }

                let shared_prefix = requested_norm
                    .chars()
                    .zip(cand_norm.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                score += shared_prefix.saturating_mul(40);

                let shared_chars = requested_norm
                    .chars()
                    .filter(|ch| cand_norm.contains(*ch))
                    .count();
                score += shared_chars.saturating_mul(12);

                let len_delta = requested_norm.len().abs_diff(cand_norm.len());
                score = score.saturating_sub(len_delta.saturating_mul(4));
                if score == 0 {
                    return None;
                }
                Some((score, idx, cand_trimmed.to_string()))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, _, candidate)| candidate)
            .collect()
    }

    async fn resolve_quorum_models(&self, policy: &QuorumPolicy) -> (Vec<String>, Vec<String>) {
        let raw = Self::collect_quorum_models(policy, &self.current_model);
        if raw.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut notes = Vec::new();
        let mut resolved = Vec::new();
        for raw_target in raw {
            let normalized = Self::normalize_quorum_model_target(&self.current_model, &raw_target);
            let (provider, model_id) = Self::split_provider_model(&normalized);
            let provider = provider.trim().to_ascii_lowercase();
            let model_id = model_id.trim();
            if provider.is_empty() || model_id.is_empty() {
                continue;
            }
            let mut final_target = normalized.clone();
            let catalog = provider_model_ids(&provider).await;
            if !catalog.is_empty() {
                if let Some(candidate) = Self::resolve_quorum_catalog_candidate(model_id, &catalog)
                {
                    final_target = format!("{}:{}", provider, candidate.trim());
                    if !final_target.eq_ignore_ascii_case(&normalized) {
                        notes.push(format!(
                            "quorum model remapped via catalog: {} -> {}",
                            normalized, final_target
                        ));
                    }
                } else if Self::looks_like_version_pinned_model(model_id) {
                    notes.push(format!(
                        "quorum model preserved despite catalog miss: {}",
                        normalized
                    ));
                } else if let Some(fallback) = catalog.first() {
                    let ranked = Self::rank_catalog_candidates(model_id, &catalog, 3);
                    final_target = format!("{}:{}", provider, fallback.trim());
                    notes.push(format!(
                        "quorum model not in provider catalog: {} ; fallback -> {} ; close matches: {}",
                        normalized,
                        final_target,
                        if ranked.is_empty() {
                            "(none)".to_string()
                        } else {
                            ranked.join(", ")
                        }
                    ));
                }
            }
            if !resolved
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&final_target))
            {
                resolved.push(final_target);
            }
        }
        (resolved, notes)
    }

    fn quorum_output_char_cap() -> Option<usize> {
        if let Ok(raw) = std::env::var("HERMES_QUORUM_MAX_VOTER_OUTPUT_CHARS") {
            if Self::is_unbounded_token(&raw) {
                return None;
            }
            if let Some(parsed) = raw.trim().parse::<usize>().ok().filter(|v| *v > 0) {
                return Some(parsed);
            }
        }
        Some(QUORUM_MAX_VOTER_OUTPUT_CHARS)
    }

    fn load_quorum_agent_contract_text(&self) -> Option<(PathBuf, String)> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(raw) = std::env::var("HERMES_QUORUM_AGENT_CONTRACT_PATH") {
            let path = PathBuf::from(raw.trim());
            if !path.as_os_str().is_empty() {
                candidates.push(path);
            }
        }
        candidates.push(self.state_root.join("quorum").join("AGENTS.md"));
        candidates.push(PathBuf::from(QUORUM_AGENT_CONTRACT_DEFAULT_PATH));
        for path in candidates {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Some((path, trimmed.to_string()));
        }
        None
    }

    fn build_quorum_voter_prompt(pass_index: usize, total_passes: usize, model: &str) -> String {
        if pass_index == 0 {
            return format!(
                "[QUORUM_VOTER] model={}\n\
                 You are in deep-voter mode. Act like quality is existential.\n\
                 Hard requirements:\n\
                 1) exhaustive exploration before conclusion,\n\
                 2) contradiction/null-hypothesis attack,\n\
                 3) final synthesis with explicit confidence and risk caveats,\n\
                 4) no placeholder names, no fake files, no invented metrics.\n\
                 Verification requirements:\n\
                 - every file/module claim must include an absolute path and exists_now=true/false\n\
                 - if you cannot verify a claim, mark it UNPROVEN (never guess)\n\
                 - include evidence bullets from tools/data/reasoning traces\n\
                 - include at least one counter-argument before final answer.\n\
                 Language requirement: answer in English unless the user explicitly requests another language.\n\
                 This is pass {}/{}.",
                model,
                pass_index + 1,
                total_passes
            );
        }
        format!(
            "[QUORUM_VOTER_REVIEW] pass {}/{}\n\
             Critique and strengthen your prior answer.\n\
             - Assume the previous draft is partially wrong.\n\
             - Remove any unverified file names/modules/metrics.\n\
             - Fix weak claims, tighten evidence, and improve actionability.\n\
             - Keep the answer in English unless the user explicitly requested another language.\n\
             - Keep objective truth over optimism.",
            pass_index + 1,
            total_passes
        )
    }

    fn extract_last_assistant_output(messages: &[hermes_core::Message]) -> String {
        for message in messages.iter().rev() {
            if message.role != hermes_core::MessageRole::Assistant {
                continue;
            }
            if let Some(content) = message.content.as_deref() {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
            if let Some(reasoning) = message.reasoning_content.as_deref() {
                let trimmed = reasoning.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        String::new()
    }

    fn truncate_for_quorum(text: &str, max_chars: Option<usize>) -> String {
        let Some(max_chars) = max_chars else {
            return text.to_string();
        };
        if max_chars == 0 || text.chars().count() <= max_chars {
            return text.to_string();
        }
        let keep = max_chars.saturating_sub(1);
        let mut out = String::with_capacity(max_chars + 24);
        for ch in text.chars().take(keep) {
            out.push(ch);
        }
        out.push('…');
        out
    }

    fn build_quorum_synthesis_prompt(
        policy: &QuorumPolicy,
        voter_outcomes: &[QuorumVoterOutcome],
    ) -> String {
        let required_success = Self::required_quorum_success(voter_outcomes.len());
        let mut prompt = String::new();
        prompt.push_str(
            "[QUORUM_SYNTHESIS] You must synthesize across independent model voters.\n\
             Rules:\n\
             1) Use only the voter outputs below as evidence.\n\
             2) Call out disagreements explicitly.\n\
             3) If a voter failed, mark it failed and continue.\n\
             4) Return: (a) strongest case, (b) strongest counter-case, (c) final synthesis with confidence.\n\
             5) Do not claim quorum executed unless voter outputs are present.\n\
             6) Reject placeholder names/fake files/fake metrics; keep only verified claims.\n\
             7) Any file claim in final synthesis must include absolute path + exists_now status or be marked UNPROVEN.\n",
        );
        prompt.push_str(
            "             8) Do not invent commands, tool calls, benchmark results, repository paths, execution evidence, or research citations.\n\
             9) Only cite a command/file/result if it appears verbatim in the voter output or the original user prompt; otherwise mark it UNPROVEN.\n\
             10) If voter evidence is thin or failed, say that directly instead of filling the gap.\n",
        );
        prompt.push_str(&format!(
            "Configured voters: {} | mode={} | enabled={} | required_success={}\n\n",
            policy.voters, policy.mode, policy.enabled, required_success
        ));
        for (idx, voter) in voter_outcomes.iter().enumerate() {
            prompt.push_str(&format!(
                "=== VOTER {} ===\nmodel: {}\nstatus: {}\nduration_ms: {}\nturns: {}\ntool_errors: {}\n",
                idx + 1,
                voter.model,
                voter.status,
                voter.duration_ms,
                voter.total_turns,
                voter.tool_errors
            ));
            if let Some(err) = &voter.error {
                prompt.push_str("error:\n");
                prompt.push_str(err);
                prompt.push('\n');
            }
            prompt.push_str("output:\n");
            prompt.push_str(&voter.output);
            prompt.push_str("\n\n");
        }
        prompt
    }

    fn persist_quorum_artifact(
        &self,
        policy: &QuorumPolicy,
        voter_outcomes: &[QuorumVoterOutcome],
    ) -> Result<PathBuf, AgentError> {
        let dir = self.state_root.join("quorum");
        std::fs::create_dir_all(&dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create quorum artifact dir {}: {}",
                dir.display(),
                e
            ))
        })?;
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let file_name = format!("{}-{}.json", self.session_id, timestamp);
        let path = dir.join(file_name);
        let payload = serde_json::json!({
            "session_id": self.session_id,
            "saved_at": chrono::Utc::now().to_rfc3339(),
            "policy": policy,
            "model_at_start": self.current_model,
            "voters": voter_outcomes,
        });
        let raw = serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Config(format!("Failed to serialize quorum artifact: {e}")))?;
        std::fs::write(&path, raw).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        Ok(path)
    }

    fn update_quorum_artifact_with_synthesis(
        path: &Path,
        synthesis: &str,
    ) -> Result<(), AgentError> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            AgentError::Io(format!(
                "Failed to read quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        let mut payload: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            AgentError::Config(format!(
                "Failed to parse quorum artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        payload["synthesis"] = serde_json::Value::String(synthesis.trim().to_string());
        payload["synthesis_saved_at"] = serde_json::Value::String(chrono::Utc::now().to_rfc3339());
        let updated = serde_json::to_string_pretty(&payload).map_err(|e| {
            AgentError::Config(format!(
                "Failed to serialize quorum synthesis artifact {}: {}",
                path.display(),
                e
            ))
        })?;
        std::fs::write(path, updated).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write quorum synthesis artifact {}: {}",
                path.display(),
                e
            ))
        })
    }

    fn apply_explore_first_runtime_defaults(config: &GatewayConfig) {
        if std::env::var("HERMES_SKILL_GUARD_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_SKILL_GUARD_MODE", "off");
        }
        if std::env::var("HERMES_GUARD_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_GUARD_MODE", "off");
        }
        if std::env::var("HERMES_TOOL_POLICY_PRESET")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_TOOL_POLICY_PRESET", "dev");
        }
        if std::env::var("HERMES_TOOL_POLICY_MODE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_TOOL_POLICY_MODE", "audit");
        }
        if std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
        if std::env::var("HERMES_MAX_ITERATIONS")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_MAX_ITERATIONS", "250");
        }
        if std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY")
            .ok()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            std::env::set_var("HERMES_TOOL_CALL_MAX_CONCURRENCY", "12");
        }
        if config.delegation.max_spawn_depth.is_none()
            && std::env::var("HERMES_MAX_DELEGATE_DEPTH")
                .ok()
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        {
            std::env::set_var("HERMES_MAX_DELEGATE_DEPTH", "4");
        }
    }

    /// Create a new `App` from the parsed CLI arguments.
    ///
    /// This loads (or creates) the gateway configuration, builds a tool
    /// registry with the configured tools, constructs an LLM provider,
    /// and initializes the agent loop.
    pub async fn new(cli: Cli) -> Result<Self, AgentError> {
        let state_root = state_dir(cli.config_dir.as_deref().map(std::path::Path::new));
        let config = load_config(cli.config_dir.as_deref())
            .map_err(|e| AgentError::Config(e.to_string()))?;

        let mut config = config;
        apply_cli_runtime_overrides(&mut config, &cli);
        Self::apply_explore_first_runtime_defaults(&config);

        if config.sessions.auto_prune {
            let resolved_home = config
                .home_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var("HERMES_HOME")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(hermes_home_dir);
            let sp = SessionPersistence::new(&resolved_home);
            let maintenance = sp.maybe_auto_prune_and_vacuum(
                config.sessions.retention_days,
                config.sessions.min_interval_hours,
                config.sessions.vacuum_after_prune,
            );
            if let Some(err) = maintenance.error {
                tracing::debug!("sessions db auto-maintenance skipped: {}", err);
            } else if !maintenance.skipped && maintenance.pruned > 0 {
                tracing::info!(
                    "sessions db auto-maintenance pruned {} session(s){}",
                    maintenance.pruned,
                    if maintenance.vacuumed {
                        " + vacuum"
                    } else {
                        ""
                    }
                );
            }
        }

        let configured_model = config
            .model
            .clone()
            .unwrap_or_else(|| "gpt-5.5".to_string());
        let current_model = resolve_startup_model(&config, &configured_model);
        let current_model = select_startup_model_with_fallback_and_auth_resolver(
            &config,
            &current_model,
            Some(&provider_oauth_token_from_auth_state),
        )
        .selected_model;
        let current_personality = config.personality.clone();

        sync_runtime_model_env(&config, &current_model);

        let tool_registry = Arc::new(ToolRegistry::new());
        if default_rtk_raw_mode() {
            tool_registry.set_raw_mode(true);
        }
        let stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>> =
            Arc::new(StdMutex::new(None));
        let terminal_backend = build_terminal_backend(&config);
        let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
        let skill_provider: Arc<dyn hermes_core::SkillProvider> =
            Arc::new(SkillManager::new(skill_store));
        hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
        wire_stdio_clarify_backend(&tool_registry);
        let cron_data_dir = state_root.join("cron");
        std::fs::create_dir_all(&cron_data_dir)
            .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_data_dir.display(), e)))?;
        let cron_scheduler = Arc::new(build_runtime_cron_scheduler(
            &config,
            &current_model,
            cron_data_dir,
            &tool_registry,
        ));
        cron_scheduler
            .load_persisted_jobs()
            .await
            .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
        cron_scheduler.start().await;
        wire_cron_scheduler_backend(&tool_registry, cron_scheduler.clone());
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let tool_schemas =
            hermes_tool_planning::resolve_platform_tool_schemas(&config, "cli", &tool_registry);

        let session_id = Uuid::new_v4().to_string();
        let mut agent_config = build_agent_config(&config, &current_model);
        agent_config.session_id = Some(session_id.clone());
        let provider = build_provider(&config, &current_model);

        let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
            agent_config,
            agent_tool_registry,
            provider,
        ))
        .with_callbacks(Self::stream_callbacks(stream_handle_shared.clone()));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            state_root.clone(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        let recovered_background_jobs = recover_queued_background_jobs(8);
        if recovered_background_jobs > 0 {
            tracing::info!(
                "Recovered {} queued background job(s) from durable status queue",
                recovered_background_jobs
            );
        }

        let app = Self {
            state_root,
            config: Arc::new(config),
            agent,
            tool_registry,
            cron_scheduler,
            tool_schemas,
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id,
            running: true,
            current_model,
            last_usage: None,
            session_usage: None,
            session_cost_usd: 0.0,
            current_personality,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared,
            mouse_enabled: default_mouse_enabled(),
            pending_theme: None,
            pending_image_hint: None,
            session_objective: None,
            pending_input_prefill: None,
            pending_system_notes: Vec::new(),
            quorum_armed_once: false,
            pet_settings: load_pet_settings(),
            #[cfg(test)]
            fail_model_rebuild_for: None,
        };
        app.ensure_session_stub_snapshot();
        Ok(app)
    }

    /// Attach a streaming handle (used by TUI mode).
    pub fn set_stream_handle(&mut self, handle: Option<StreamHandle>) {
        if let Ok(mut guard) = self.stream_handle_shared.lock() {
            *guard = handle.clone();
        }
        self.stream_handle = handle;
    }

    /// Enable/disable TUI mouse handling.
    pub fn set_mouse_enabled(&mut self, enabled: bool) {
        self.mouse_enabled = enabled;
    }

    /// Current TUI mouse handling state.
    pub fn mouse_enabled(&self) -> bool {
        self.mouse_enabled
    }

    /// Queue a TUI skin/theme change request to be applied in the UI loop.
    pub fn request_theme_change(&mut self, skin: &str) {
        let value = skin.trim();
        if value.is_empty() {
            return;
        }
        self.pending_theme = Some(value.to_string());
    }

    /// Queue an image hint for the next user prompt.
    pub fn set_pending_image_hint(&mut self, path: String) {
        let trimmed = path.trim();
        self.pending_image_hint = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    /// Read queued image hint without consuming it.
    pub fn pending_image_hint(&self) -> Option<&str> {
        self.pending_image_hint.as_deref()
    }

    /// Clear queued image hint.
    pub fn clear_pending_image_hint(&mut self) {
        self.pending_image_hint = None;
    }

    /// Submit text through the normal user-message path and run the agent.
    pub async fn submit_user_message(&mut self, raw: &str) -> Result<(), AgentError> {
        for note in std::mem::take(&mut self.pending_system_notes) {
            self.messages.push(hermes_core::Message::system(note));
        }
        let user_message = self.prepare_user_message(raw);
        self.messages.push(hermes_core::Message::user(user_message));
        self.run_agent().await
    }

    pub fn queue_next_turn_system_note(&mut self, note: String) {
        let trimmed = note.trim();
        if !trimmed.is_empty() {
            self.pending_system_notes.push(trimmed.to_string());
        }
    }

    #[cfg(test)]
    pub fn pending_system_note_count(&self) -> usize {
        self.pending_system_notes.len()
    }

    pub fn take_pending_input_prefill(&mut self) -> Option<String> {
        self.pending_input_prefill.take()
    }

    fn composer_drafts_path(&self) -> PathBuf {
        self.state_root.join(COMPOSER_DRAFTS_FILE)
    }

    fn composer_draft_key(&self) -> String {
        let trimmed = self.session_id.trim();
        if trimmed.is_empty() {
            "__new__".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn load_composer_draft_store(&self) -> Result<ComposerDraftStore, AgentError> {
        let path = self.composer_drafts_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ComposerDraftStore {
                    version: 1,
                    drafts: Vec::new(),
                });
            }
            Err(err) => {
                return Err(AgentError::Io(format!(
                    "Failed to read composer drafts {}: {}",
                    path.display(),
                    err
                )));
            }
        };
        let mut store: ComposerDraftStore = serde_json::from_str(&raw).map_err(|err| {
            AgentError::Config(format!(
                "Failed to parse composer drafts {}: {}",
                path.display(),
                err
            ))
        })?;
        store.version = 1;
        Ok(store)
    }

    fn write_composer_draft_store(&self, store: &ComposerDraftStore) -> Result<(), AgentError> {
        let path = self.composer_drafts_path();
        if store.drafts.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(AgentError::Io(format!(
                        "Failed to remove composer drafts {}: {}",
                        path.display(),
                        err
                    )));
                }
            }
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AgentError::Io(format!(
                    "Failed to create composer draft dir {}: {}",
                    parent.display(),
                    err
                ))
            })?;
        }
        let raw = serde_json::to_string_pretty(store).map_err(|err| {
            AgentError::Config(format!("Failed to serialize composer drafts: {err}"))
        })?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, raw).map_err(|err| {
            AgentError::Io(format!(
                "Failed to write composer drafts {}: {}",
                tmp_path.display(),
                err
            ))
        })?;
        std::fs::rename(&tmp_path, &path).map_err(|err| {
            AgentError::Io(format!(
                "Failed to replace composer drafts {}: {}",
                path.display(),
                err
            ))
        })?;
        Ok(())
    }

    /// Load unsent composer text for the active session.
    pub fn load_current_composer_draft(&self) -> Result<Option<String>, AgentError> {
        let key = self.composer_draft_key();
        let store = self.load_composer_draft_store()?;
        Ok(store
            .drafts
            .into_iter()
            .rev()
            .find(|draft| draft.session_id == key && !draft.text.trim().is_empty())
            .map(|draft| draft.text))
    }

    /// Persist unsent composer text for the active session.
    pub fn persist_current_composer_draft(&self, text: &str) -> Result<(), AgentError> {
        let key = self.composer_draft_key();
        let mut store = self.load_composer_draft_store()?;
        store.drafts.retain(|draft| draft.session_id != key);
        if !text.trim().is_empty() {
            store.drafts.push(ComposerDraftRecord {
                session_id: key,
                text: text.to_string(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            });
        }
        if store.drafts.len() > MAX_COMPOSER_DRAFTS {
            let keep_from = store.drafts.len() - MAX_COMPOSER_DRAFTS;
            store.drafts.drain(0..keep_from);
        }
        store.version = 1;
        self.write_composer_draft_store(&store)
    }

    /// Clear unsent composer text for the active session.
    pub fn clear_current_composer_draft(&self) -> Result<(), AgentError> {
        self.persist_current_composer_draft("")
    }

    /// Prepare outbound user text, consuming any queued image hint.
    pub fn prepare_user_message(&mut self, raw: &str) -> String {
        let base = raw.trim();
        if let Some(path) = self
            .pending_image_hint
            .take()
            .filter(|value| !value.trim().is_empty())
        {
            format!("[IMAGE_HINT] path={}\n{}", path, base)
        } else {
            base.to_string()
        }
    }

    /// Drain any queued skin/theme change request.
    pub fn take_pending_theme_change(&mut self) -> Option<String> {
        self.pending_theme.take()
    }

    /// Retrieve current companion pet settings.
    pub fn pet_settings(&self) -> &PetSettings {
        &self.pet_settings
    }

    /// Update and persist companion pet settings.
    pub fn set_pet_settings(&mut self, settings: PetSettings) -> Result<(), AgentError> {
        let normalized = settings.normalized();
        persist_pet_settings(&normalized)?;
        self.pet_settings = normalized;
        Ok(())
    }

}

include!("app_impl_interaction.rs");
