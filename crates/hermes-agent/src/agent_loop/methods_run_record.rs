const RUN_FINALIZATION_DROP_GRACE: Duration = Duration::from_secs(1);

/// Ensures that an owner lease is not silently abandoned when a public run
/// future is cancelled after its durable run record has started.
struct RunFinalizationGuard {
    active_run: Arc<ActiveRun>,
    terminal_reason: Option<TerminalReason>,
    armed: bool,
}

impl RunFinalizationGuard {
    fn new(active_run: Arc<ActiveRun>) -> Self {
        Self {
            active_run,
            terminal_reason: None,
            armed: true,
        }
    }

    fn record_terminal_reason(&mut self, terminal_reason: TerminalReason) {
        self.terminal_reason = Some(terminal_reason);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RunFinalizationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let terminal_reason = self.terminal_reason.unwrap_or_else(|| {
            if std::thread::panicking() {
                TerminalReason::ExecutionFailure
            } else {
                TerminalReason::Cancellation
            }
        });
        let active_run = Arc::clone(&self.active_run);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Without a live executor there is no safe place to finish. Keep
            // the owner lease untouched for durable stale-run recovery.
            return;
        };
        runtime.spawn(async move {
            let _ = tokio::time::timeout(
                RUN_FINALIZATION_DROP_GRACE,
                finish_active_run_with_retry(active_run.as_ref(), terminal_reason),
            )
            .await;
        });
    }
}

fn terminal_reason_for_result(result: &Result<AgentResult, AgentError>) -> TerminalReason {
    match result {
        Ok(result) if result.interrupted => TerminalReason::Cancellation,
        Ok(result) if result.finished_naturally => TerminalReason::Completed,
        Ok(_) => TerminalReason::ExecutionFailure,
        Err(AgentError::PolicyBlocked(_)) => TerminalReason::PolicyBlocked,
        Err(AgentError::Timeout(_)) => TerminalReason::Timeout,
        Err(AgentError::Interrupted { .. }) => TerminalReason::Cancellation,
        Err(_) => TerminalReason::ExecutionFailure,
    }
}

async fn finish_active_run_with_retry(
    active_run: &ActiveRun,
    terminal_reason: TerminalReason,
) -> Result<(), String> {
    let mut last_error = None;
    for attempt in 0..3 {
        match active_run.finish(terminal_reason).await {
            Ok(_) => return Ok(()),
            Err(error) => last_error = Some(error.to_string()),
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(25 * (attempt + 1))).await;
        }
    }
    Err(last_error.unwrap_or_else(|| "unknown finalization failure".to_string()))
}

impl AgentLoop {
    /// Resolve every private execution input exactly once before a run record is
    /// admitted. Both streaming paths consume this same owned bundle.
    async fn prepare_run_input(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<PreparedRunInput, AgentError> {
        let mut initial_messages = messages;
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let context_length = get_model_context_length(&self.config.model);
        for message in &mut initial_messages {
            if message.role != MessageRole::User {
                continue;
            }
            let Some(content) = message.content.clone() else {
                continue;
            };
            let result =
                preprocess_context_references_async(&content, &cwd, context_length, None).await;
            if result.expanded && result.message != content {
                message.content = Some(result.message);
            }
        }

        let mut advertised_tools = tools.unwrap_or_else(|| self.tool_registry.schemas());
        if self.config.skip_memory || !self.tool_registry.canonical_memory_bound() {
            advertised_tools.retain(|schema| schema.name != "memory");
        } else {
            let canonical = self
                .tool_registry
                .get("memory")
                .ok_or_else(|| {
                    AgentError::Config(
                        "canonical memory binding is marked active but the tool is absent"
                            .to_string(),
                    )
                })?
                .schema
                .clone();
            let mut memory_count = 0_u8;
            for schema in &advertised_tools {
                if schema.name == "memory" {
                    memory_count = memory_count.saturating_add(1);
                    if schema != &canonical {
                        return Err(AgentError::Config(
                            "advertised memory schema does not match the subject-bound canonical tool"
                                .to_string(),
                        ));
                    }
                }
            }
            if memory_count > 1 {
                return Err(AgentError::Config(
                    "canonical memory tool may be advertised at most once".to_string(),
                ));
            }
        }
        let (initial_messages, advertised_tools) =
            PreparedRunInput::sanitize_execution_inputs(initial_messages, advertised_tools)?;
        let task_hint = initial_messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.clone())
            .unwrap_or_default();
        let mut system_prompt =
            self.resolve_initial_system_prompt(&task_hint, &advertised_tools);
        PreparedRunInput::sanitize_system_prompt(&mut system_prompt);
        Ok(PreparedRunInput::new(
            initial_messages,
            advertised_tools,
            task_hint,
            system_prompt,
        ))
    }

    /// Policy rejections still get an auditable terminal run, but must not
    /// trigger prompt construction, skill discovery, or prompt-side hooks.
    fn prepare_policy_blocked_run_input(&self, messages: Vec<Message>) -> PreparedRunInput {
        let mut initial_messages = messages;
        sanitize_prepared_messages(&mut initial_messages);
        PreparedRunInput::new(
            initial_messages,
            Vec::new(),
            String::new(),
            ResolvedSystemPrompt::unavailable("policy_rejected_before_prompt_resolution"),
        )
    }


    /// Resolve an OAuth credential only after the run exists, and only through
    /// the active run's durable one-shot gate. The gate deduplicates retries
    /// and route re-entry for the same provider within this run.
    async fn resolve_governed_oauth_before_model_call(
        &self,
        active_run: Option<&Arc<ActiveRun>>,
        effective_provider: &str,
    ) -> Result<(), AgentError> {
        let Some(provider) =
            Self::oauth_refresh_provider_for_provider(effective_provider)
        else {
            return Ok(());
        };
        let Some(active_run) = active_run else {
            return Ok(());
        };
        let refresh_provider = provider.clone();
        active_run
            .execute_oauth_refresh_once(&provider, || {
                self.refresh_runtime_oauth_credential_for_active_run(&refresh_provider)
            })
            .await
            .map_err(|error| error.into_agent_error())
    }

    fn effective_provider_for_model_call(
        &self,
        model: &str,
        route: Option<&TurnRuntimeRoute>,
    ) -> String {
        route
            .and_then(|route| route.provider.as_deref())
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.extract_provider_and_model(model).0)
    }

    fn start_active_run_record(
        &self,
        prepared: &PreparedRunInput,
    ) -> Result<Arc<ActiveRun>, AgentError> {
        let runtime = self.run_runtime.as_ref().ok_or_else(|| {
            AgentError::Config(
                "agent execution requires an attached run-record runtime".to_string(),
            )
        })?;
        let containment = self.execution_containment.as_deref().ok_or_else(|| {
            AgentError::Config(
                "closed execution requires an attached certified containment context".to_string(),
            )
        })?;
        let registry_containment = self.tool_registry.execution_containment_context();
        if !std::ptr::eq(containment, registry_containment.as_ref()) {
            return Err(AgentError::Config(
                "agent loop and tool registry do not share the same containment context"
                    .to_string(),
            ));
        }
        runtime
            .start_run(
                &self.config,
                containment,
                &self.tool_registry,
                prepared.start_evidence(),
                &self.run_context,
                self.parent_run_id,
            )
            .map_err(|error| AgentError::Io(format!("run-record start failed: {error}")))
    }

    fn validate_governed_callbacks(&self) -> Result<(), AgentError> {
        if self.config.background_review_enabled {
            return Err(AgentError::PolicyBlocked(
                "background review requires durable post-commit scheduling and is unavailable"
                    .to_string(),
            ));
        }
        if !self.config.skip_memory {
            if let Some(memory_manager) = self.memory_manager.as_ref() {
                let memory_manager = memory_manager.lock().map_err(|_| {
                    AgentError::PolicyBlocked(
                        "memory manager lock is poisoned during governed-run preflight".to_string(),
                    )
                })?;
                if !memory_manager.has_trusted_builtin_memory_tool() {
                    return Err(AgentError::PolicyBlocked(
                        "effect-capable memory manager is unavailable until it has the trusted canonical builtin binding required for typed run intents"
                            .to_string(),
                    ));
                }
            }
        }
        if let Some(plugin_manager) = self.plugin_manager.as_ref() {
            let mut plugin_manager = plugin_manager.lock().map_err(|_| {
                AgentError::PolicyBlocked(
                    "plugin manager lock is poisoned during governed-run preflight".to_string(),
                )
            })?;
            let has_ungoverned_callbacks = plugin_manager.has_effect_capable_callbacks();
            if has_ungoverned_callbacks {
                return Err(AgentError::PolicyBlocked(
                    "effect-capable plugin hooks or middleware are unavailable until they carry typed provenance"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }


    async fn complete_active_run_record(
        &self,
        active_run: &Arc<ActiveRun>,
        result: &Result<AgentResult, AgentError>,
    ) -> Result<(), AgentError> {
        let terminal_reason = terminal_reason_for_result(result);
        let (outcome, evidence) = match result {
            Ok(result) if result.interrupted => {
                ("handler_error", "agent run was interrupted".to_string())
            }
            Ok(result) if result.finished_naturally => {
                let evidence = result
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == MessageRole::Assistant)
                    .and_then(|message| message.content.clone())
                    .unwrap_or_else(|| "agent completed without assistant text".to_string());
                ("returned", evidence)
            }
            Ok(_) => (
                "handler_error",
                "agent stopped before natural completion".to_string(),
            ),
            Err(AgentError::PolicyBlocked(error)) => ("handler_error", error.clone()),
            Err(AgentError::Timeout(error)) => ("handler_error", error.clone()),
            Err(AgentError::Interrupted { message }) => (
                "handler_error",
                message
                    .clone()
                    .unwrap_or_else(|| "agent run was interrupted".to_string()),
            ),
            Err(error) => ("handler_error", error.to_string()),
        };

        if let Err(record_error) = active_run.record_agent_outcome(outcome, &evidence).await {
            let finish_error =
                finish_active_run_with_retry(active_run.as_ref(), TerminalReason::ExecutionFailure)
                    .await
                    .err();
            return Err(AgentError::Io(match finish_error {
                Some(finish_error) => format!(
                    "run-record output append failed: {record_error}; finalization also failed: {finish_error}"
                ),
                None => format!("run-record output append failed: {record_error}"),
            }));
        }
        finish_active_run_with_retry(active_run.as_ref(), terminal_reason)
            .await
            .map_err(|error| AgentError::Io(format!("run-record finalization failed: {error}")))
    }
}
