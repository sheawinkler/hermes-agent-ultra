use crate::media::stage_media_delivery_path;

impl Gateway {
    /// Non-streaming message routing: invoke agent, send complete response.
    async fn route_non_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
        lease: &TurnLease,
        delivery_started: &AtomicBool,
    ) -> Result<(), GatewayError> {
        self.emit_turn_start(incoming, session_key, lease, false)
            .await?;
        let deferred_messages = Arc::new(StdMutex::new(Vec::new()));
        let deferred_release = Arc::new(AtomicBool::new(false));
        let mut runtime_context = self
            .build_runtime_context(incoming, session_key, Some(lease))
            .await;
        runtime_context.deferred_post_delivery_messages = Some(deferred_messages.clone());
        runtime_context.deferred_post_delivery_released = Some(deferred_release.clone());
        let context_handler = self.message_handler_with_context.read().await.clone();
        if !self.is_turn_current(lease).await {
            return Err(GatewayError::SessionExpired(
                "gateway turn was reset before handler start".to_string(),
            ));
        }
        let turn_timeout_secs = self.config.turn_timeout_secs.max(1);
        let response_result = match tokio::time::timeout(
            Duration::from_secs(turn_timeout_secs),
            async {
                if let Some(handler) = context_handler {
                    handler(messages, runtime_context).await
                } else {
                    let handler = self.message_handler.read().await.clone();
                    let handler = handler.ok_or_else(|| {
                        GatewayError::Platform("No message handler configured".into())
                    })?;
                    let messages = self.inject_runtime_hints(session_key, messages).await;
                    handler(messages).await
                }
            },
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let _ = self.busy_sessions.write().await.interrupt_if_current(
                    lease,
                    "Gateway turn deadline exceeded; stop active agent and tool work.",
                );
                Err(GatewayError::Platform(format!(
                    "gateway turn timed out after {turn_timeout_secs}s"
                )))
            }
        };
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                let turn_lock = self.session_turn_lock(session_key).await;
                let error_guard = turn_lock.lock().await;
                let busy = self.busy_sessions.read().await;
                if !busy.is_current(lease) {
                    return Err(GatewayError::SessionExpired(
                        "gateway turn was reset before error lifecycle commit".to_string(),
                    ));
                }
                drop(busy);
                let _ = tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.emit_hook_event(
                        "agent:end",
                        serde_json::json!({
                            "platform": incoming.platform,
                            "chat_id": incoming.chat_id,
                            "user_id": incoming.user_id,
                            "session_id": session_key,
                            "streaming": false,
                            "success": false,
                            "error": e.to_string()
                        }),
                    ),
                )
                .await;
                drop(error_guard);
                return Err(e);
            }
        };

        let reply_thread_id = Self::reply_thread_id(incoming);
        let turn_lock = self.session_turn_lock(session_key).await;
        let commit_guard = turn_lock.lock().await;
        let busy = self.busy_sessions.read().await;
        if !busy.is_current(lease) {
            return Err(GatewayError::SessionExpired(
                "gateway turn was reset before response commit".to_string(),
            ));
        }
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;
        self.bump_output_usage(session_key, response.chars().count())
            .await;
        drop(busy);

        // Once the outbound call starts, cancellation or transport failure
        // cannot prove Discord/the adapter did not accept the response. Do
        // not impose a second 30-second umbrella timeout here: every typed
        // adapter operation below has its own finite delivery bound, and that
        // bound must be allowed to return PartialDelivery/ambiguous evidence.
        delivery_started.store(true, Ordering::Release);
        self.send_notify_message_threaded(
            &incoming.platform,
            &incoming.chat_id,
            &response,
            None,
            reply_thread_id,
        )
        .await?;
        self.flush_post_delivery_messages(
            &incoming.platform,
            &incoming.chat_id,
            reply_thread_id,
            deferred_messages,
            deferred_release,
        )
        .await;
        let _ = tokio::time::timeout(
            SESSION_COMMIT_TIMEOUT,
            self.emit_hook_event(
                "agent:end",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key,
                    "streaming": false,
                    "success": true,
                    "response_chars": response.chars().count()
                }),
            ),
        )
        .await;
        drop(commit_guard);

        Ok(())
    }

    /// Streaming message routing: progressively edit messages as tokens arrive.
    async fn route_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
        lease: &TurnLease,
        delivery_started: &AtomicBool,
    ) -> Result<(), GatewayError> {
        self.emit_turn_start(incoming, session_key, lease, true)
            .await?;
        let deferred_messages = Arc::new(StdMutex::new(Vec::new()));
        let deferred_release = Arc::new(AtomicBool::new(false));
        let mut runtime_context = self
            .build_runtime_context(incoming, session_key, Some(lease))
            .await;
        runtime_context.deferred_post_delivery_messages = Some(deferred_messages.clone());
        runtime_context.deferred_post_delivery_released = Some(deferred_release.clone());
        let context_handler = self.streaming_handler_with_context.read().await.clone();
        let legacy_messages = self
            .inject_runtime_hints(session_key, messages.clone())
            .await;

        let stream_turn_lock = self.session_turn_lock(session_key).await;
        let start_guard = stream_turn_lock.lock().await;
        let busy = self.busy_sessions.read().await;
        if !busy.is_current(lease) {
            return Err(GatewayError::SessionExpired(
                "gateway turn was reset before streaming start".to_string(),
            ));
        }
        let stream_handle = self
            .stream_manager
            .start_stream(&incoming.platform, &incoming.chat_id)
            .await;
        let stream_id = stream_handle.id.clone();
        let reply_thread_id = Self::reply_thread_id(incoming).map(str::to_string);
        drop(busy);
        delivery_started.store(true, Ordering::Release);
        let anchor_result = match AssertUnwindSafe(tokio::time::timeout(
            SESSION_COMMIT_TIMEOUT,
            self.send_message_threaded(
                &incoming.platform,
                &incoming.chat_id,
                "...",
                None,
                reply_thread_id.as_deref(),
            ),
        ))
        .catch_unwind()
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(GatewayError::Platform(
                "gateway streaming anchor delivery timed out".to_string(),
            )),
            Err(_) => Err(GatewayError::Platform(
                "gateway streaming anchor delivery panicked".to_string(),
            )),
        };
        drop(start_guard);
        if let Err(error) = anchor_result {
            self.stream_manager.finish_stream(&stream_id).await;
            return Err(error);
        }

        // Serialize chunk updates through one supervised worker. This avoids
        // spawning one task per token while preserving ordered stream edits.
        let stream_manager = self.stream_manager.clone();
        let platform = incoming.platform.clone();
        let chat_id = incoming.chat_id.clone();
        let thread_id = reply_thread_id.clone();
        let gateway_adapters = self.adapters.read().await.clone();
        let busy_sessions = self.busy_sessions.clone();
        let turn_lease = lease.clone();
        let chunk_turn_lock = stream_turn_lock.clone();
        let sid = stream_id.clone();
        let shutting_down = self.shutting_down.clone();
        let chunk_mailbox = Arc::new(StreamChunkMailbox::default());
        let chunk_mailbox_closer = StreamChunkMailboxCloser(chunk_mailbox.clone());
        let worker_mailbox = chunk_mailbox.clone();
        let (chunk_done_tx, chunk_done_rx) = tokio::sync::oneshot::channel::<()>();
        let (chunk_start_tx, chunk_start_rx) = tokio::sync::oneshot::channel::<()>();
        let worker_id = self.next_turn_worker_id.fetch_add(1, Ordering::Relaxed);
        let workers = self.turn_workers.clone();
        let handle = tokio::spawn(async move {
            if chunk_start_rx.await.is_ok() {
                let _ = AssertUnwindSafe(async {
                    while let Some(chunk) = worker_mailbox.recv().await {
                        if shutting_down.load(Ordering::Acquire) {
                            break;
                        }
                        let _commit_guard = chunk_turn_lock.lock().await;
                        if !busy_sessions.read().await.is_current(&turn_lease) {
                            break;
                        }
                        if let Some(should_flush) =
                            stream_manager.update_stream(&sid, &chunk).await
                        {
                            if should_flush {
                                if let Some(content) =
                                    stream_manager.get_stream_content(&sid).await
                                {
                                    if let Some(adapter) = gateway_adapters.get(&platform) {
                                        let _ = Self::await_adapter_call(
                                            "stream chunk delivery",
                                            SESSION_COMMIT_TIMEOUT,
                                            || {
                                                adapter.send_message_with_options(
                                                    &chat_id,
                                                    &content,
                                                    None,
                                                    SendMessageOptions::threaded(
                                                        thread_id.as_deref(),
                                                    ),
                                                )
                                            },
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                    }
                })
                .catch_unwind()
                .await;
                let _ = chunk_done_tx.send(());
            }
            workers.lock().unwrap().remove(&worker_id);
        });
        let mut handle = Some(handle);
        let rejected = {
            let mut registered_workers = self.turn_workers.lock().unwrap();
            if self.shutting_down.load(Ordering::Acquire) {
                true
            } else {
                registered_workers.insert(
                    worker_id,
                    handle.take().expect("stream worker handle missing"),
                );
                false
            }
        };
        if rejected {
            handle
                .take()
                .expect("rejected stream worker handle missing")
                .abort();
            self.stream_manager.finish_stream(&stream_id).await;
            return Err(GatewayError::SessionExpired(
                "gateway is shutting down".to_string(),
            ));
        }
        if chunk_start_tx.send(()).is_err() {
            if let Some(handle) = self.turn_workers.lock().unwrap().remove(&worker_id) {
                handle.abort();
            }
            self.stream_manager.finish_stream(&stream_id).await;
            return Err(GatewayError::Platform(
                "stream chunk worker ended before startup".to_string(),
            ));
        }
        let callback_mailbox = chunk_mailbox.clone();
        let on_chunk: Arc<dyn Fn(String) + Send + Sync> =
            Arc::new(move |chunk: String| callback_mailbox.push(chunk));

        // Invoke the streaming handler
        if !self.is_turn_current(lease).await {
            self.stream_manager.finish_stream(&stream_id).await;
            return Err(GatewayError::SessionExpired(
                "gateway turn was reset before streaming handler start".to_string(),
            ));
        }
        let turn_timeout_secs = self.config.turn_timeout_secs.max(1);
        let mut response_result = match tokio::time::timeout(
            Duration::from_secs(turn_timeout_secs),
            AssertUnwindSafe(async {
                if let Some(handler) = context_handler {
                    handler(messages, runtime_context, on_chunk).await
                } else {
                    let handler = self.streaming_handler.read().await.clone();
                    match handler {
                        Some(handler) => handler(legacy_messages, on_chunk).await,
                        None => Err(GatewayError::Platform(
                            "No streaming handler configured".into(),
                        )),
                    }
                }
            })
            .catch_unwind(),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(GatewayError::Platform(
                "gateway streaming handler panicked".to_string(),
            )),
            Err(_) => {
                let _ = self.busy_sessions.write().await.interrupt_if_current(
                    lease,
                    "Gateway streaming turn deadline exceeded; stop active agent and tool work.",
                );
                Err(GatewayError::Platform(format!(
                    "gateway streaming turn timed out after {turn_timeout_secs}s"
                )))
            }
        };
        chunk_mailbox_closer.0.close();
        let chunk_drained = matches!(
            tokio::time::timeout(SESSION_COMMIT_TIMEOUT, chunk_done_rx).await,
            Ok(Ok(()))
        );
        if !chunk_drained {
            if let Some(handle) = self.turn_workers.lock().unwrap().remove(&worker_id) {
                handle.abort();
            }
            if response_result.is_ok() {
                response_result = Err(GatewayError::Platform(
                    "stream chunk worker did not drain before final delivery".to_string(),
                ));
            }
        }
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                self.stream_manager.finish_stream(&stream_id).await;
                let error_guard = stream_turn_lock.lock().await;
                let busy = self.busy_sessions.read().await;
                if !busy.is_current(lease) {
                    return Err(GatewayError::SessionExpired(
                        "gateway turn was reset before streaming error lifecycle commit"
                            .to_string(),
                    ));
                }
                drop(busy);
                let _ = tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.emit_hook_event(
                        "agent:end",
                        serde_json::json!({
                            "platform": incoming.platform,
                            "chat_id": incoming.chat_id,
                            "user_id": incoming.user_id,
                            "session_id": session_key,
                            "streaming": true,
                            "success": false,
                            "error": e.to_string()
                        }),
                    ),
                )
                .await;
                drop(error_guard);
                return Err(e);
            }
        };

        // Finish the stream
        self.stream_manager.finish_stream(&stream_id).await;
        let commit_guard = stream_turn_lock.lock().await;
        let busy = self.busy_sessions.read().await;
        if !busy.is_current(lease) {
            return Err(GatewayError::SessionExpired(
                "gateway turn was reset before streaming response commit".to_string(),
            ));
        }
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;
        self.bump_output_usage(session_key, response.chars().count())
            .await;
        drop(busy);
        // Final delivery owns typed, finite adapter deadlines. A 30-second
        // wrapper would cancel a compound Discord forum/upload operation
        // after an accepted prefix and discard its PartialDelivery evidence.
        if !response.trim().is_empty() {
            self.send_notify_message_threaded(
                &incoming.platform,
                &incoming.chat_id,
                &response,
                None,
                reply_thread_id.as_deref(),
            )
            .await?;
        }
        self.flush_post_delivery_messages(
            &incoming.platform,
            &incoming.chat_id,
            reply_thread_id.as_deref(),
            deferred_messages,
            deferred_release,
        )
        .await;
        let _ = tokio::time::timeout(
            SESSION_COMMIT_TIMEOUT,
            self.emit_hook_event(
                "agent:end",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key,
                    "streaming": true,
                    "success": true,
                    "response_chars": response.chars().count()
                }),
            ),
        )
        .await;
        drop(commit_guard);

        Ok(())
    }

    async fn emit_turn_start(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        lease: &TurnLease,
        streaming: bool,
    ) -> Result<(), GatewayError> {
        let turn_lock = self.session_turn_lock(session_key).await;
        let start_guard = turn_lock.lock().await;
        let busy = self.busy_sessions.read().await;
        if !busy.is_current(lease) {
            return Err(GatewayError::SessionExpired(
                "gateway turn was reset before start lifecycle commit".to_string(),
            ));
        }
        drop(busy);
        tokio::time::timeout(
            SESSION_COMMIT_TIMEOUT,
            self.emit_hook_event(
                "agent:start",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key,
                    "streaming": streaming
                }),
            ),
        )
        .await
        .map_err(|_| {
            GatewayError::Platform("gateway start lifecycle hook timed out".to_string())
        })?;
        drop(start_guard);
        Ok(())
    }

    async fn inject_runtime_hints(
        &self,
        session_key: &str,
        messages: Vec<Message>,
    ) -> Vec<Message> {
        let default_model = self.default_model.read().await.clone();
        let (state, pending_system_notes) = {
            let mut states = self.runtime_state.write().await;
            let state = states.entry(session_key.to_string()).or_default();
            let pending = std::mem::take(&mut state.pending_system_notes);
            (state.clone(), pending)
        };

        let mut hints = Vec::new();
        if let Some(model) = state.model.or(default_model) {
            hints.push(format!("model={}", model));
        }
        if let Some(provider) = state.provider {
            hints.push(format!("provider={}", provider));
        }
        if let Some(profile) = state.profile {
            hints.push(format!("profile={}", profile));
        }
        if let Some(branch) = state.branch {
            hints.push(format!("branch={}", branch));
        }
        if let Some(service_tier) = state
            .service_tier
            .or_else(|| normalize_service_tier(self.config.service_tier.as_deref()))
        {
            hints.push(format!("service_tier={service_tier}"));
        }
        if hints.is_empty() && pending_system_notes.is_empty() {
            return messages;
        }

        let mut out = Vec::with_capacity(messages.len() + 1 + pending_system_notes.len());
        if !hints.is_empty() {
            out.push(Message::system(format!(
                "[gateway_runtime]\n{}",
                hints.join("\n")
            )));
        }
        for note in pending_system_notes {
            out.push(Message::system(note));
        }
        out.extend(messages);
        out
    }

    async fn build_runtime_context(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        lease: Option<&TurnLease>,
    ) -> GatewayRuntimeContext {
        let default_model = self.default_model.read().await.clone();
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let mcp_reload_generation = *self.mcp_reload_generation.read().await;
        let session_id = self
            .session_manager
            .get_session(session_key)
            .await
            .map(|session| session.id);

        GatewayRuntimeContext {
            session_key: session_key.to_string(),
            platform: incoming.platform.clone(),
            tenant_id: (!incoming.platform.trim().is_empty())
                .then(|| incoming.platform.clone()),
            // IncomingMessage has no authenticated team field. Do not infer
            // one from a group chat id and accidentally widen visibility.
            team_id: None,
            session_id,
            is_dm: incoming.is_dm,
            chat_id: incoming.chat_id.clone(),
            thread_id: incoming.thread_id.clone(),
            user_id: incoming.user_id.clone(),
            model: state.model.or(default_model),
            provider: state.provider,
            profile: state.profile,
            branch: state.branch,
            personality: state.personality,
            home: state.home,
            service_tier: state
                .service_tier
                .or_else(|| normalize_service_tier(self.config.service_tier.as_deref())),
            tool_progress: state.tool_progress,
            verbose: state.verbose,
            yolo: state.yolo,
            reasoning: state.reasoning,
            mcp_reload_generation,
            busy_control: lease.map(|lease| {
                BusyControlRegistration::new(lease.clone(), self.busy_sessions.clone())
            }),
            deferred_post_delivery_messages: None,
            deferred_post_delivery_released: None,
        }
    }
    fn reply_thread_id(incoming: &IncomingMessage) -> Option<&str> {
        incoming
            .thread_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }

    async fn flush_post_delivery_messages(
        &self,
        platform: &str,
        chat_id: &str,
        thread_id: Option<&str>,
        pending: Arc<StdMutex<Vec<String>>>,
        released: Arc<AtomicBool>,
    ) {
        released.store(true, Ordering::Release);
        let queued = match pending.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(_) => Vec::new(),
        };
        for message in queued {
            if let Err(e) = self
                .send_message_threaded(platform, chat_id, &message, None, thread_id)
                .await
            {
                warn!(
                    platform = platform,
                    chat_id = chat_id,
                    error = %e,
                    "Failed to flush deferred post-delivery message"
                );
            }
        }
    }

    async fn send_turn_bound_message(
        &self,
        incoming: &IncomingMessage,
        lease: &TurnLease,
        text: &str,
    ) -> Result<(), GatewayError> {
        let turn_lock = self.session_turn_lock(lease.session_key()).await;
        let _commit_guard = turn_lock.lock().await;
        let busy = self.busy_sessions.read().await;
        if !busy.is_current(lease) {
            return Err(GatewayError::SessionExpired(
                "gateway turn was reset before message delivery".to_string(),
            ));
        }
        drop(busy);
        tokio::time::timeout(
            SESSION_COMMIT_TIMEOUT,
            self.send_message(&incoming.platform, &incoming.chat_id, text, None),
        )
        .await
        .map_err(|_| GatewayError::Platform("gateway message delivery timed out".to_string()))?
    }

    async fn bump_input_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.user_messages += 1;
        stat.input_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    async fn bump_output_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.assistant_messages += 1;
        stat.output_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    async fn build_usage_text(&self, session_key: &str) -> String {
        let usage = self.usage_stats.read().await;
        let stat = usage.get(session_key).cloned().unwrap_or_default();
        let approx_input_tokens = stat.input_chars / 4;
        let approx_output_tokens = stat.output_chars / 4;
        let mut text = format!(
            "📊 Usage\n- user messages: {}\n- assistant messages: {}\n- input chars: {} (~{} tokens)\n- output chars: {} (~{} tokens)",
            stat.user_messages,
            stat.assistant_messages,
            stat.input_chars,
            approx_input_tokens,
            stat.output_chars,
            approx_output_tokens
        );
        let nous_credits = hermes_core::credits::render_last_nous_credits_lines();
        if !nous_credits.is_empty() {
            text.push_str("\n\n");
            text.push_str(&nous_credits.join("\n"));
        }
        text
    }

    fn summarize_removed_messages(messages: &[Message]) -> Result<String, String> {
        let mut bullets = Vec::new();
        for msg in messages {
            let Some(raw) = msg.content.as_ref() else {
                continue;
            };
            let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            if compact.is_empty() {
                continue;
            }
            let truncated = if compact.chars().count() > 180 {
                let mut head = compact.chars().take(177).collect::<String>();
                head.push_str("...");
                head
            } else {
                compact
            };
            bullets.push(format!("• {}: {}", role_label(msg.role), truncated));
            if bullets.len() >= 6 {
                break;
            }
        }

        if bullets.is_empty() {
            return Err("no textual content available to summarize".to_string());
        }

        let mut out =
            String::from("[CONTEXT COMPACTION] Earlier conversation was compacted. Key points:\n");
        out.push_str(&bullets.join("\n"));
        Ok(out)
    }

    async fn compress_context(
        &self,
        session_key: &str,
        max_messages: usize,
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
    ) -> CompressionOutcome {
        let current = self.session_manager.get_messages(session_key).await;
        if current.len() <= max_messages {
            return CompressionOutcome::default();
        }

        let mut compressed = Vec::new();
        let mut head_count = 0usize;
        if let Some(first) = current.first() {
            if first.role == MessageRole::System {
                compressed.push(first.clone());
                head_count = 1;
            }
        }
        let keep_tail = max_messages.saturating_sub(compressed.len());
        let mut tail: Vec<Message> = current.iter().rev().take(keep_tail).cloned().collect();
        tail.reverse();
        let tail_start = current.len().saturating_sub(keep_tail);
        let middle = if tail_start > head_count {
            &current[head_count..tail_start]
        } else {
            &[]
        };
        let removed_messages = middle.len();

        let mut summary_warning = None;
        if removed_messages > 0 {
            match Self::summarize_removed_messages(middle) {
                Ok(summary) => compressed.push(Message::assistant(&summary)),
                Err(err) => {
                    compressed.push(Message::assistant(format!(
                        "[CONTEXT COMPACTION] Summary generation was unavailable. {removed_messages} message(s) were removed to free context space but could not be summarized. Continue from recent messages and current workspace state."
                    )));
                    summary_warning = Some(format!(
                        "⚠️ Context compression summary failed ({err}). {removed_messages} historical message(s) were removed and replaced with a placeholder."
                    ));
                }
            }
        }
        compressed.extend(tail);

        let replaced = self
            .session_manager
            .replace_messages(session_key, compressed)
            .await;
        if replaced {
            self.commit_command_dedup(dedup_key, dedup_claim).await;
        }
        CompressionOutcome {
            removed_messages,
            summary_warning,
        }
    }

    async fn build_status_text(&self, session_key: &str) -> String {
        let default_model = self.default_model.read().await.clone();
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let usage = self
            .usage_stats
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let title = self.session_manager.get_title(session_key).await;
        let messages = self.session_manager.get_messages(session_key).await;
        let running_tasks = self
            .background_tasks
            .list_tasks()
            .into_iter()
            .filter(|(_, status, _)| *status == TaskStatus::Running)
            .count();
        let (busy_active, busy_pending) = {
            let busy = self.busy_sessions.read().await;
            (busy.is_active(session_key), busy.pending_len(session_key))
        };

        format!(
            "🧭 Gateway status\n- title: {}\n- model: {}\n- provider: {}\n- profile: {}\n- branch: {}\n- personality: {}\n- service tier: {}\n- reasoning: {}\n- verbose: {}\n- tool progress: {}\n- busy input: {}\n- busy ack: {}\n- busy active: {}\n- busy queued: {}\n- yolo: {}\n- home: {}\n- messages in session: {}\n- running background tasks: {}\n- mcp generation: {}\n- input/output chars: {}/{}",
            title.unwrap_or_else(|| "(untitled)".to_string()),
            state
                .model
                .or(default_model)
                .unwrap_or_else(|| "default".to_string()),
            state.provider.unwrap_or_else(|| "default".to_string()),
            state.profile.unwrap_or_else(|| "default".to_string()),
            state.branch.unwrap_or_else(|| "main".to_string()),
            state.personality.unwrap_or_else(|| "default".to_string()),
            state
                .service_tier
                .or_else(|| normalize_service_tier(self.config.service_tier.as_deref()))
                .unwrap_or_else(|| "default".to_string()),
            if state.reasoning { "ON" } else { "OFF" },
            if state.verbose { "ON" } else { "OFF" },
            state.tool_progress.unwrap_or_else(|| "default".to_string()),
            self.config.display.normalized_busy_input_mode(),
            if self.config.display.busy_ack_enabled() {
                "ON"
            } else {
                "OFF"
            },
            if busy_active { "yes" } else { "no" },
            busy_pending,
            if state.yolo { "ON" } else { "OFF" },
            state.home.unwrap_or_else(|| "(not set)".to_string()),
            messages.len(),
            running_tasks,
            *self.mcp_reload_generation.read().await,
            usage.input_chars,
            usage.output_chars
        )
    }

    async fn handle_background_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        prompt: &str,
        isolated_context: bool,
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
    ) -> Result<bool, GatewayError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(GatewayError::Platform(
                "gateway is shutting down".to_string(),
            ));
        }
        let trimmed = prompt.trim();
        if trimmed.eq_ignore_ascii_case("list") {
            let tasks = self.background_tasks.list_tasks();
            let summary = if tasks.is_empty() {
                "No background tasks.".to_string()
            } else {
                let mut out = String::from("🧵 Background tasks:\n");
                for (id, status, task_prompt) in tasks {
                    out.push_str(&format!("- {} [{:?}] {}\n", id, status, task_prompt));
                }
                out
            };
            self.send_message(&incoming.platform, &incoming.chat_id, &summary, None)
                .await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("cancel ").map(str::trim) {
            let ok = self.background_tasks.cancel(task_id);
            if ok {
                self.commit_command_dedup(dedup_key, dedup_claim).await;
            }
            let msg = if ok {
                format!("Cancelled background task {}", task_id)
            } else {
                format!("Task {} was not running or not found", task_id)
            };
            self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                .await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("status ").map(str::trim) {
            let msg = match self.background_tasks.get_status(task_id) {
                Some(TaskStatus::Running) => format!("Task {} is running", task_id),
                Some(TaskStatus::Completed) => {
                    let result = self
                        .background_tasks
                        .get_result(task_id)
                        .unwrap_or_default();
                    format!("Task {} completed.\n{}", task_id, result)
                }
                Some(TaskStatus::Failed(err)) => format!("Task {} failed: {}", task_id, err),
                Some(TaskStatus::Cancelled) => format!("Task {} was cancelled", task_id),
                None => format!("Task {} not found", task_id),
            };
            self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                .await?;
            return Ok(true);
        }

        let legacy_handler = self.message_handler.read().await.as_ref().cloned();
        let context_handler = self
            .message_handler_with_context
            .read()
            .await
            .as_ref()
            .cloned();
        if context_handler.is_none() && legacy_handler.is_none() {
            return Err(GatewayError::Platform(
                "No message handler configured".into(),
            ));
        }

        let task_id = if isolated_context {
            Self::python_async_task_id("btw")
        } else {
            Self::python_async_task_id("bg")
        };

        let preview = Self::gateway_command_preview(trimmed);
        let ack = if isolated_context {
            format!("💬 /btw: \"{}\"\nReply will appear here shortly.", preview)
        } else {
            format!(
                "🔄 Background task started: \"{}\"\nTask ID: {}\nYou can keep chatting — results will appear when done.",
                preview, task_id
            )
        };

        let manager = self.background_tasks.clone();
        let task_id_for_task = task_id.clone();
        let adapters = self.adapters.read().await.clone();
        let platform = incoming.platform.clone();
        let chat_id = incoming.chat_id.clone();
        let thread_id = Self::reply_thread_id(incoming).map(str::to_string);
        let notify_task_id = task_id.clone();
        // Python `GatewayRunner._run_background_task`: only `user_message=prompt` (fresh session).
        // Python `_run_btw_task`: `conversation_history` snapshot + ephemeral user turn (no tools).
        let original_messages = if isolated_context {
            let mut history = self.session_manager.get_messages(session_key).await;
            let btw_user = format!(
                "[Ephemeral /btw side question. Answer using the conversation \
                 context. No tools available. Be direct and concise.]\n\n{}",
                trimmed
            );
            history.push(Message::user(btw_user));
            history
        } else {
            vec![Message::user(trimmed)]
        };
        let legacy_messages = original_messages.clone();
        let runtime_context = self.build_runtime_context(incoming, session_key, None).await;
        let turn_timeout_secs = self.config.turn_timeout_secs.max(1);
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(GatewayError::Platform(
                "gateway is shutting down".to_string(),
            ));
        }
        self.background_tasks
            .submit_with_id(task_id.clone(), trimmed.to_string())
            .map_err(GatewayError::Platform)?;
        let shutting_down = self.shutting_down.clone();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _worker_guard = manager.worker_guard(task_id_for_task.clone());
            if start_rx.await.is_err() {
                manager.fail(
                    &task_id_for_task,
                    "background startup abandoned".to_string(),
                );
                return;
            }
            if shutting_down.load(Ordering::Acquire) {
                manager.fail(
                    &task_id_for_task,
                    "gateway is shutting down".to_string(),
                );
                return;
            }
            if let Some(adapter) = adapters.get(&platform) {
                if let Err(error) = Self::await_adapter_call(
                    "background start acknowledgement",
                    NONCRITICAL_ADAPTER_TIMEOUT,
                    || {
                        adapter.send_message_with_options(
                            &chat_id,
                            &ack,
                            None,
                            SendMessageOptions::notify_threaded(thread_id.as_deref()),
                        )
                    },
                )
                .await
                {
                    warn!(
                        platform = %platform,
                        chat_id = %chat_id,
                        task_id = %task_id_for_task,
                        error = %error,
                        "failed to acknowledge accepted background task; work remains owned"
                    );
                }
            }
            let result = match tokio::time::timeout(
                Duration::from_secs(turn_timeout_secs),
                AssertUnwindSafe(async {
                    if let Some(handler) = context_handler {
                        handler(original_messages, runtime_context).await
                    } else if let Some(handler) = legacy_handler {
                        handler(legacy_messages).await
                    } else {
                        Err(GatewayError::Platform(
                            "No message handler configured".into(),
                        ))
                    }
                })
                .catch_unwind(),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(GatewayError::Platform(
                    "background task handler panicked".to_string(),
                )),
                Err(_) => Err(GatewayError::Platform(format!(
                    "background task timed out after {turn_timeout_secs}s"
                ))),
            };

            match result {
                Ok(result) => {
                    if !manager.complete(&task_id_for_task, result.clone()) {
                        return;
                    }
                    if shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some(adapter) = adapters.get(&platform) {
                        let prefix = if isolated_context {
                            "💬 /btw result".to_string()
                        } else {
                            format!("✅ Background task {notify_task_id} completed")
                        };
                        let notification = format!("{prefix}:\n{result}");
                        let _ = Self::await_adapter_call(
                            "background completion delivery",
                            SESSION_COMMIT_TIMEOUT,
                            || {
                                adapter.send_message_with_options(
                                    &chat_id,
                                    &notification,
                                    None,
                                    SendMessageOptions::notify_threaded(
                                        thread_id.as_deref(),
                                    ),
                                )
                            },
                        )
                        .await;
                    }
                }
                Err(err) => {
                    let error = err.to_string();
                    if !manager.fail(&task_id_for_task, error.clone()) {
                        return;
                    }
                    if shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some(adapter) = adapters.get(&platform) {
                        let prefix = if isolated_context {
                            "❌ /btw failed".to_string()
                        } else {
                            format!("❌ Background task {notify_task_id} failed")
                        };
                        let notification = format!("{prefix}: {error}");
                        let _ = Self::await_adapter_call(
                            "background failure delivery",
                            SESSION_COMMIT_TIMEOUT,
                            || {
                                adapter.send_message_with_options(
                                    &chat_id,
                                    &notification,
                                    None,
                                    SendMessageOptions::notify_threaded(
                                        thread_id.as_deref(),
                                    ),
                                )
                            },
                        )
                        .await;
                    }
                }
            }
        });
        if !self.background_tasks.register_handle(&task_id, handle) {
            self.background_tasks.cancel(&task_id);
            return Err(GatewayError::Platform(
                "background task was cancelled before startup".to_string(),
            ));
        }
        self.commit_command_dedup(dedup_key, dedup_claim).await;
        if start_tx.send(()).is_err() {
            self.background_tasks
                .fail(&task_id, "background worker ended before startup".to_string());
            return Err(GatewayError::Platform(
                "background worker ended before startup".to_string(),
            ));
        }
        Ok(true)
    }

    fn dedup_key(incoming: &IncomingMessage) -> Option<String> {
        let message_id = incoming.message_id.as_deref()?.trim();
        if message_id.is_empty() {
            return None;
        }
        Some(format!(
            "{}:{}:{}",
            incoming.platform.trim().to_ascii_lowercase(),
            incoming.chat_id.trim(),
            message_id
        ))
    }

    async fn acquire_dedup_claim(&self, incoming: &IncomingMessage) -> DedupIngressClaim {
        let Some(key) = Self::dedup_key(incoming) else {
            return DedupIngressClaim::Untracked;
        };
        loop {
            let outcome = self.message_deduplicator.lock().claim(key.clone());
            match outcome {
                DedupClaimOutcome::Owner => {
                    return DedupIngressClaim::Owner {
                        guard: DedupClaimGuard::new(
                            self.message_deduplicator.clone(),
                            key.clone(),
                        ),
                        key,
                    };
                }
                DedupClaimOutcome::Completed => return DedupIngressClaim::Completed,
                DedupClaimOutcome::InFlight(receiver) => match receiver.await {
                    Ok(DedupResolution::Completed) => return DedupIngressClaim::Completed,
                    Ok(DedupResolution::Retry) | Err(_) => continue,
                },
            }
        }
    }

    async fn complete_dedup_key(&self, key: Option<&str>) {
        let Some(key) = key else {
            return;
        };
        self.message_deduplicator.lock().complete(key);
    }

    async fn release_dedup_key(&self, key: Option<&str>) {
        let Some(key) = key else {
            return;
        };
        self.message_deduplicator.lock().forget(key);
    }

    async fn complete_dedup_keys(&self, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        let mut deduplicator = self.message_deduplicator.lock();
        for key in keys {
            deduplicator.complete(key);
        }
    }
    async fn release_dedup_keys(&self, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        let mut deduplicator = self.message_deduplicator.lock();
        for key in keys {
            deduplicator.forget(key);
        }
    }


    /// `preview = prompt[:60] + ("..." if len(prompt) > 60 else "")` (Python gateway).
    fn gateway_command_preview(prompt: &str) -> String {
        let t = prompt.trim();
        let mut it = t.chars();
        let head: String = it.by_ref().take(60).collect();
        if it.next().is_some() {
            format!("{}...", head)
        } else {
            head
        }
    }

    /// Python: `f"{kind}_{%H%M%S}_{os.urandom(3).hex()}"` style task ids (`bg_…`, `btw_…`).
    fn python_async_task_id(kind: &str) -> String {
        let ts = chrono::Utc::now().format("%H%M%S");
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.subsec_nanos() as u64) ^ d.as_secs().wrapping_mul(0x9e37_79b9_85f0_a7b5))
            .unwrap_or(0xABCDEF);
        format!("{}_{}_{:06x}", kind, ts, salt & 0xFFFFFF)
    }

    fn extract_command_name(text: &str) -> Option<String> {
        let trimmed = text.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }
        let token = trimmed[1..].split_whitespace().next()?.trim();
        if token.is_empty() {
            return None;
        }
        Some(token.to_ascii_lowercase())
    }

    // -----------------------------------------------------------------------
    // Message sending (delegates to adapters)
    // -----------------------------------------------------------------------

    /// Send a text message to a specific platform chat.
    pub async fn send_message(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_message_threaded(platform, chat_id, text, parse_mode, None)
            .await
    }

    /// Continue a durably recorded provider suffix. Non-retryable records are
    /// rejected by the adapter rather than risking a duplicate visible reply.
    pub async fn resume_partial_delivery(
        &self,
        partial: &hermes_core::errors::PartialDelivery,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(&partial.platform).await.ok_or_else(|| {
            GatewayError::Platform(format!(
                "No adapter registered for platform: {}",
                partial.platform
            ))
        })?;
        Self::await_adapter_call(
            "partial message delivery",
            SESSION_COMMIT_TIMEOUT,
            || adapter.resume_partial_delivery(partial),
        )
        .await
    }

    /// Send a text message to a specific platform chat, optionally preserving
    /// the platform-native thread id for threaded platforms such as Slack.
    pub async fn send_message_threaded(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_message_with_options(
            platform,
            chat_id,
            text,
            parse_mode,
            SendMessageOptions::threaded(thread_id),
        )
        .await
    }

    /// Send a final user-visible reply, allowing adapters to make final-answer
    /// delivery choices distinct from operational progress/status sends.
    pub async fn send_notify_message_threaded(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_message_with_options(
            platform,
            chat_id,
            text,
            parse_mode,
            SendMessageOptions::notify_threaded(thread_id),
        )
        .await
    }

    /// Send a message to a caller-supplied platform chat/topic.
    ///
    /// This keeps explicit tool targets distinct from gateway-origin replies
    /// that may intentionally use platform-level home/publish overrides.
    pub async fn send_message_explicit(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_message_explicit_with_audit_label(
            platform, chat_id, text, parse_mode, thread_id, None,
        )
        .await
    }

    pub async fn send_message_explicit_with_audit_label(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
        audit_label: Option<&str>,
    ) -> Result<(), GatewayError> {
        let mut options = SendMessageOptions::explicit_threaded(thread_id);
        if let Some(label) = audit_label {
            options = options.with_delivery_audit_label(label.to_string());
        }
        self.send_message_with_options(platform, chat_id, text, parse_mode, options)
            .await
    }

    async fn await_adapter_call<T, F, M>(
        operation: &str,
        timeout: Duration,
        make_future: M,
    ) -> Result<T, GatewayError>
    where
        F: Future<Output = Result<T, GatewayError>>,
        M: FnOnce() -> F,
    {
        match tokio::time::timeout(
            timeout,
            AssertUnwindSafe(async move { make_future().await }).catch_unwind(),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(GatewayError::Platform(format!(
                "platform adapter panicked during {operation}"
            ))),
            Err(_) => Err(GatewayError::Platform(format!(
                "platform adapter timed out during {operation}"
            ))),
        }
    }

    async fn send_adapter_text_with_options(
        adapter: &Arc<dyn PlatformAdapter>,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let prepared = std::panic::catch_unwind(AssertUnwindSafe(|| {
            prepare_platform_message_for_adapter(
                adapter.as_ref(),
                text,
                options.delivery_audit_label.as_deref(),
            )
        }))
        .map_err(|_| {
            GatewayError::Platform(
                "platform adapter panicked while preparing message delivery".to_string(),
            )
        })??;
        // A platform's `send_message_with_options` can be compound (Discord
        // chunks/forum starter+follow-up). Give it the same finite bound as
        // the longest native transfer so it can return typed acceptance or
        // partial-delivery evidence to the gateway.
        Self::await_adapter_call("message delivery", ADAPTER_FILE_TIMEOUT, || {
            adapter.send_message_with_options(
                chat_id,
                prepared.as_ref(),
                parse_mode,
                options,
            )
        })
        .await
    }

    async fn send_message_with_options(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        let (without_media, media_files) = extract_media_markers(text);
        let (cleaned, images) = extract_inline_images(&without_media);
        if images.is_empty() && media_files.is_empty() {
            if without_media == text {
                return Self::send_adapter_text_with_options(
                    &adapter, chat_id, text, parse_mode, options,
                )
                .await;
            }
            return Self::send_adapter_text_with_options(
                &adapter, chat_id, &cleaned, parse_mode, options,
            )
            .await;
        }

        if !cleaned.is_empty() {
            Self::send_adapter_text_with_options(
                &adapter,
                chat_id,
                &cleaned,
                parse_mode.clone(),
                options.clone(),
            )
            .await?;
        }

        for image in images {
            let Err(err) = Self::await_adapter_call(
                "image delivery",
                SESSION_COMMIT_TIMEOUT,
                || {
                    adapter.send_image_url_with_options(
                        chat_id,
                        &image.url,
                        image.alt_text.as_deref(),
                        options.clone(),
                    )
                },
            )
            .await
            else {
                continue;
            };
            if !matches!(
                adapter.classify_native_media_failure(&err),
                NativeMediaFailureClass::Unsupported | NativeMediaFailureClass::Rejected
            ) {
                return Err(err);
            }

            warn!(
                platform = platform,
                chat_id = chat_id,
                image_url = %image.url,
                error = %err,
                "native image send was conclusively rejected; falling back to plain URL message"
            );
            let fallback = match image.alt_text.as_deref().map(str::trim) {
                Some(caption) if !caption.is_empty() => format!("{caption}\n{}", image.url),
                _ => image.url.clone(),
            };
            #[cfg(feature = "discord")]
            let fallback = if platform.eq_ignore_ascii_case("discord") {
                crate::platforms::discord::discord_image_fallback_text(
                    &image.url,
                    image.alt_text.as_deref(),
                )
            } else {
                fallback
            };
            Self::send_adapter_text_with_options(
                &adapter,
                chat_id,
                &fallback,
                Some(ParseMode::Plain),
                options.clone(),
            )
            .await?;
        }

        for media in media_files {
            let Some(validated_delivery) = stage_media_delivery_path(&media.path).await else {
                warn!(
                    platform = platform,
                    chat_id = chat_id,
                    media_path = %media.path,
                    as_voice = media.as_voice,
                    "refusing to deliver unsafe MEDIA marker path"
                );
                Self::send_adapter_text_with_options(
                    &adapter,
                    chat_id,
                    "[media attachment blocked: unsafe local file path]",
                    Some(ParseMode::Plain),
                    options.clone(),
                )
                .await?;
                continue;
            };
            let Some(validated_path) = validated_delivery.staged_path().to_str() else {
                warn!(
                    platform = platform,
                    chat_id = chat_id,
                    media_path = %media.path,
                    as_voice = media.as_voice,
                    "refusing to deliver non-UTF-8 MEDIA marker path"
                );
                Self::send_adapter_text_with_options(
                    &adapter,
                    chat_id,
                    "[media attachment blocked: non-UTF-8 local file path]",
                    Some(ParseMode::Plain),
                    options.clone(),
                )
                .await?;
                continue;
            };

            let media_name = Path::new(validated_path)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("attachment");
            let media_options = options
                .clone()
                .with_voice_delivery(media.as_voice);
            let Err(err) = Self::await_adapter_call(
                "media file delivery",
                ADAPTER_FILE_TIMEOUT,
                || {
                    adapter.send_file_with_options(
                        chat_id,
                        validated_path,
                        None,
                        media_options.clone(),
                    )
                },
            )
            .await
            else {
                continue;
            };
            if !matches!(
                adapter.classify_native_media_failure(&err),
                NativeMediaFailureClass::Unsupported | NativeMediaFailureClass::Rejected
            ) {
                return Err(err);
            }
            let media_notice = match &err {
                GatewayError::MediaPreflightFailed { file_name, code } => {
                    format!("[media attachment unavailable: {file_name} ({code})]")
                }
                _ => format!("[media attachment unavailable: {media_name}]"),
            };

            warn!(
                platform = platform,
                chat_id = chat_id,
                media_file = media_name,
                as_voice = media.as_voice,
                error = %err,
                "native media file send was conclusively rejected; falling back to safe attachment notice"
            );
            Self::send_adapter_text_with_options(
                &adapter,
                chat_id,
                &media_notice,
                Some(ParseMode::Plain),
                options.clone(),
            )
            .await?;
        }

        Ok(())
    }

    /// Edit an existing message on a specific platform chat.
    pub async fn edit_message(
        &self,
        platform: &str,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        Self::await_adapter_call("message edit", SESSION_COMMIT_TIMEOUT, || {
            adapter.edit_message(chat_id, message_id, text)
        })
        .await
    }

    /// Send a status update, editing an existing status bubble when the
    /// platform supports keyed status messages.
    pub async fn send_or_update_status(
        &self,
        platform: &str,
        chat_id: &str,
        status_key: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_or_update_status_threaded(
            platform,
            chat_id,
            status_key,
            text,
            parse_mode,
            None,
        )
        .await
    }

    /// Send a status update to an optional native thread/topic target.
    ///
    /// The parent chat remains the portable routing key; adapters that model a
    /// thread as a separate channel may route the status directly to it.
    pub async fn send_or_update_status_threaded(
        &self,
        platform: &str,
        chat_id: &str,
        status_key: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        let options = SendMessageOptions::non_conversational_threaded(thread_id);
        Self::await_adapter_call("status update", SESSION_COMMIT_TIMEOUT, || {
            adapter.send_or_update_status_with_options(
                chat_id,
                status_key,
                text,
                parse_mode.clone(),
                options.clone(),
            )
        })
        .await
    }

    /// Send a file to a specific platform chat with an optional caption.
    pub async fn send_file(
        &self,
        platform: &str,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        let validated_delivery = stage_media_delivery_path(file_path).await.ok_or_else(|| {
            GatewayError::SendFailed("Refusing to deliver unsafe local file path".to_string())
        })?;
        let validated_path = validated_delivery.staged_path().to_str().ok_or_else(|| {
            GatewayError::SendFailed("Refusing to deliver non-UTF-8 local file path".to_string())
        })?;
        Self::await_adapter_call("file delivery", ADAPTER_FILE_TIMEOUT, || {
            adapter.send_file(chat_id, validated_path, caption)
        })
        .await
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Get a reference to the session manager.
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the stream manager.
    pub fn stream_manager(&self) -> &Arc<StreamManager> {
        &self.stream_manager
    }

    /// Get a reference to the gateway config.
    pub fn config(&self) -> &GatewayConfig {
        self.config.as_ref()
    }

    /// List the names of all registered adapters.
    pub async fn adapter_names(&self) -> Vec<String> {
        self.adapters.read().await.keys().cloned().collect()
    }

    /// Periodically expires inactive sessions.
    pub async fn session_expiry_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(30)));
        loop {
            tokio::select! {
                biased;
                _ = self.wait_for_shutdown() => return,
                _ = ticker.tick() => {
                    if self.shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    let expired = self.expire_idle_sessions_once("idle_expiry").await;
                    if expired > 0 {
                        tracing::info!(expired, "Expired idle sessions");
                    }
                }
            }
        }
    }

    /// Expire idle sessions once and emit lifecycle finalization hooks for
    /// each removed session using retained session snapshots.
    pub async fn expire_idle_sessions_once(&self, reason: &str) -> usize {
        if self.shutting_down.load(Ordering::Acquire) {
            return 0;
        }
        let candidates = self.session_manager.idle_session_keys().await;
        let mut expired = 0usize;
        for session_key in candidates {
            if self.shutting_down.load(Ordering::Acquire) {
                break;
            }
            let Some(expiry_lease) = ({
                let mut busy = self.busy_sessions.write().await;
                busy.begin_idle_expiry(&session_key)
            }) else {
                continue;
            };

            let gateway = self.clone();
            let reason = reason.to_string();
            let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
            let worker_id = self.next_turn_worker_id.fetch_add(1, Ordering::Relaxed);
            let workers = self.turn_workers.clone();
            let registered = {
                let mut registered_workers = self.turn_workers.lock().unwrap();
                if self.shutting_down.load(Ordering::Acquire) {
                    false
                } else {
                    let handle = tokio::spawn(async move {
                        let removed = match AssertUnwindSafe(async {
                            let turn_lock = gateway.session_turn_lock(&session_key).await;
                            let _turn_guard = turn_lock.lock().await;
                            let current = gateway.session_manager.get_session(&session_key).await;
                            if let Some(session) =
                                current.as_ref().filter(|session| session.should_reset())
                            {
                                let _ = tokio::time::timeout(
                                    NONCRITICAL_ADAPTER_TIMEOUT,
                                    gateway.emit_session_finalize(&session_key, session, &reason),
                                )
                                .await;
                                let removed = gateway
                                    .session_manager
                                    .expire_idle_session_snapshot(&session_key)
                                    .await;
                                if removed.is_some() {
                                    gateway.purge_removed_session_state(&session_key).await;
                                }
                                removed
                            } else {
                                None
                            }
                        })
                        .catch_unwind()
                        .await
                        {
                            Ok(removed) => removed,
                            Err(_) => {
                                warn!(session_key, "idle-expiry worker panicked");
                                None
                            }
                        };

                        let completion = {
                            let mut busy = gateway.busy_sessions.write().await;
                            busy.complete_turn(&expiry_lease, ProcessingOutcome::Success)
                        };
                        if let TurnCompletion::Completed {
                            next: Some((lease, entry)),
                        } = completion
                        {
                            gateway.spawn_turn_chain_detached(lease, entry);
                        }
                        let _ = completion_tx.send(removed.is_some());
                        workers.lock().unwrap().remove(&worker_id);
                    });
                    registered_workers.insert(worker_id, handle);
                    true
                }
            };
            if !registered {
                break;
            }
            if completion_rx.await.unwrap_or(false) {
                expired += 1;
            }
        }
        expired
    }

    /// Monitors adapter health and attempts reconnect through stop/start.
    pub async fn platform_reconnect_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(20)));
        loop {
            ticker.tick().await;
            if self.shutting_down.load(Ordering::Acquire) {
                return;
            }
            let snapshot = self.adapters.read().await.clone();
            for (name, adapter) in snapshot {
                let running = match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    adapter.is_running()
                })) {
                    Ok(running) => running,
                    Err(_) => {
                        tracing::warn!(
                            platform = %name,
                            "Adapter panicked during health check"
                        );
                        false
                    }
                };
                let reconnectable = match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    adapter.is_reconnectable()
                })) {
                    Ok(reconnectable) => reconnectable,
                    Err(_) => {
                        tracing::warn!(
                            platform = %name,
                            "Adapter panicked while checking reconnect eligibility"
                        );
                        false
                    }
                };
                if !running && !reconnectable {
                    tracing::warn!(
                        platform = %name,
                        "Adapter is terminally offline; skipping automatic reconnect"
                    );
                    continue;
                }
                if !running {
                    tracing::warn!(platform = %name, "Adapter appears offline, reconnecting");
                    match tokio::time::timeout(
                        SESSION_COMMIT_TIMEOUT,
                        AssertUnwindSafe(async { adapter.start().await }).catch_unwind(),
                    )
                    .await
                    {
                        Ok(Ok(Ok(()))) => {}
                        Ok(Ok(Err(error))) => tracing::warn!(
                            platform = %name,
                            %error,
                            "Adapter reconnect failed"
                        ),
                        Ok(Err(_)) => tracing::warn!(
                            platform = %name,
                            "Adapter panicked during reconnect"
                        ),
                        Err(_) => tracing::warn!(
                            platform = %name,
                            "Adapter timed out during reconnect"
                        ),
                    }
                }
            }
        }
    }

    /// Attach vision hint for image-bearing messages.
    pub fn enrich_message_with_vision(&self, text: &str) -> String {
        if text.contains("http://") || text.contains("https://") {
            format!("[vision_candidate]\n{}", text)
        } else {
            text.to_string()
        }
    }

    /// Attach transcription hint for audio-bearing messages.
    pub fn enrich_message_with_transcription(&self, text: &str) -> String {
        if text.contains(".mp3") || text.contains(".wav") || text.contains(".m4a") {
            format!("[transcription_candidate]\n{}", text)
        } else {
            text.to_string()
        }
    }

    /// Build deterministic signature for config-change detection.
    pub fn agent_config_signature(&self) -> String {
        let s = serde_json::to_string(self.config.as_ref()).unwrap_or_default();
        format!("{:x}", md5::compute(s))
    }

    /// Load optional prefill messages.
    pub fn load_prefill_messages(&self, path: &std::path::Path) -> Vec<Message> {
        hermes_config::load_prefill_messages_file(path)
    }

    /// Load optional ephemeral system prompt.
    pub fn load_ephemeral_system_prompt(&self, path: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Resolve model routing candidate for a message (static heuristics only; no adaptive policy store).
    pub fn load_smart_model_routing(&self, text: &str) -> Option<String> {
        Self::message_requests_smart_model_route(text)
            .then(|| self.config.model.clone())
            .flatten()
    }

    fn message_requests_smart_model_route(text: &str) -> bool {
        text.len() > 2000
            || text.contains("analyze")
            || text.contains("refactor")
            || text.contains("quick")
            || text.contains("summary")
    }

    /// Authorize user based on DM manager and platform context.
    pub async fn is_user_authorized(&self, user_id: &str, platform: &str) -> bool {
        let dm = self.dm_manager.read().await;
        dm.is_authorized(user_id) || dm.handle_dm(user_id, platform).await == DmDecision::Allow
    }

    /// Send update notification message to a chat.
    pub async fn send_update_notification(
        &self,
        platform: &str,
        chat_id: &str,
        latest_version: &str,
    ) -> Result<(), GatewayError> {
        let msg = format!("Update available: Hermes {}", latest_version);
        self.send_message(platform, chat_id, &msg, None).await
    }

    /// Watch external process output and forward to a callback.
    pub async fn run_process_watcher(
        &self,
        mut child: tokio::process::Child,
        on_output: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<(), GatewayError> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| GatewayError::Platform("Process has no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| GatewayError::Platform(format!("Watcher read error: {}", e)))?
        {
            on_output(line);
        }
        Ok(())
    }

    async fn maybe_apply_smart_model_routing(&self, session_key: &str, text: &str) {
        let has_model = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .and_then(|s| s.model.clone())
            .is_some();
        if has_model {
            return;
        }
        if Self::message_requests_smart_model_route(text) {
            let model = self.default_model.read().await.clone();
            let model = model.or_else(|| self.config.model.clone());
            let Some(model) = model else {
                return;
            };
            let mut states = self.runtime_state.write().await;
            states.entry(session_key.to_string()).or_default().model = Some(model);
        }
    }
}
