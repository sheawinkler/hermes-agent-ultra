impl AgentLoop {
    /// Populate [`AgentConfig::stored_system_prompt`] from SQLite (`sessions.system_prompt`) for session continuation.
    ///
    /// Call before [`AgentLoop::run`] when resuming a gateway/CLI session so Anthropic prefix cache matches Python.
    pub fn hydrate_stored_system_prompt_from_hermes_home(
        config: &mut AgentConfig,
        hermes_home: &std::path::Path,
    ) -> Result<(), AgentError> {
        let Some(ref sid) = config.session_id else {
            return Ok(());
        };
        if sid.trim().is_empty() {
            return Ok(());
        }
        let sp = crate::session_persistence::SessionPersistence::new(hermes_home);
        sp.ensure_db()?;
        if let Some(prompt) = sp.get_system_prompt(sid)? {
            config.stored_system_prompt = Some(prompt);
        }
        Ok(())
    }

    fn initial_execution_containment(
        config: &AgentConfig,
    ) -> Option<Arc<ExecutionContainmentContext>> {
        match config.execution_mode.mode {
            ExecutionMode::Manual => Some(Arc::new(
                ExecutionContainmentContext::manual(config.execution_mode)
                    .expect("manual resolved mode must admit a host-local containment context"),
            )),
            ExecutionMode::Reviewed
            | ExecutionMode::AutonomousLocal
            | ExecutionMode::Ci
            | ExecutionMode::RemoteWorker => None,
        }
    }

    /// Build [`AgentResult`] messages for return / persistence (applies `persist_user_message` override).
    fn messages_for_persisted_result(
        &self,
        ctx: &ContextManager,
        persist_user_idx: Option<usize>,
        prefill_range: Option<Range<usize>>,
    ) -> Vec<Message> {
        let mut msgs = ctx.get_messages().to_vec();
        if let (Some(idx), Some(override_text)) = (
            persist_user_idx,
            self.config.persist_user_message.as_deref(),
        ) {
            if let Some(msg) = msgs.get_mut(idx) {
                if msg.role == MessageRole::User {
                    msg.content = Some(override_text.to_string());
                }
            }
        }
        if let Some(range) = prefill_range {
            if range.start <= range.end && range.end <= msgs.len() {
                msgs.drain(range);
            }
        }
        msgs
    }

    fn named_tool_result_message(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Message {
        Message::tool_result_with_name(tool_call_id, tool_name, content)
    }

    fn tool_result_message_from_execution(result: &ToolResult, tool_calls: &[ToolCall]) -> Message {
        tool_calls
            .iter()
            .find(|tc| tc.id == result.tool_call_id)
            .map(|tc| {
                Self::named_tool_result_message(
                    &result.tool_call_id,
                    &tc.function.name,
                    &result.content,
                )
            })
            .unwrap_or_else(|| Message::tool_result(&result.tool_call_id, &result.content))
    }

    fn graceful_interrupt_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermes_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        prefill_range: Option<Range<usize>>,
    ) -> AgentResult {
        self.memory_on_session_end(ctx.get_messages());
        AgentResult {
            messages: self.messages_for_persisted_result(ctx, persist_user_idx, prefill_range),
            finished_naturally: false,
            total_turns,
            tool_errors: tool_errors.to_vec(),
            usage: accumulated_usage,
            interrupted: true,
            session_cost_usd: Some(session_cost_usd),
            session_started_hooks_fired,
        }
    }

    fn drain_tool_batch_interrupt(&self) -> ToolBatchInterrupt {
        match self.interrupt.take_interrupt_graceful() {
            None => ToolBatchInterrupt::None,
            Some(Some(message)) if is_formatted_steer_marker(&message) => {
                ToolBatchInterrupt::Steer(message)
            }
            Some(_) => ToolBatchInterrupt::Stop,
        }
    }

    fn collect_tool_batch_interrupt(&self, steer_markers: &mut Vec<String>) -> bool {
        match self.drain_tool_batch_interrupt() {
            ToolBatchInterrupt::None => false,
            ToolBatchInterrupt::Stop => true,
            ToolBatchInterrupt::Steer(marker) => {
                steer_markers.push(marker);
                false
            }
        }
    }

    fn append_steer_markers_to_last_tool_result(
        results: &mut [ToolResult],
        steer_markers: &[String],
    ) {
        if steer_markers.is_empty() {
            return;
        }
        if let Some(result) = results.last_mut() {
            for marker in steer_markers {
                result.content.push_str(marker);
            }
        }
    }

    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        let route_learning = Arc::new(Mutex::new(HashMap::new()));
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        let execution_containment = Self::initial_execution_containment(&config);
        if let Some(context) = &execution_containment {
            tool_registry.set_execution_containment_context(Arc::clone(context));
        }
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            memory_manager: None,
            memory_subject: None,
            memory_subject_filter_key: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
            run_runtime: None,
            execution_containment,
            run_context: RunInvocationContext::default(),
            parent_run_id: None,
            code_index,
            lsp_context,
            route_learning,
        }
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        let route_learning = Arc::new(Mutex::new(HashMap::new()));
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        let execution_containment = Self::initial_execution_containment(&config);
        if let Some(context) = &execution_containment {
            tool_registry.set_execution_containment_context(Arc::clone(context));
        }
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
            memory_manager: None,
            memory_subject: None,
            memory_subject_filter_key: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
            run_runtime: None,
            execution_containment,
            run_context: RunInvocationContext::default(),
            parent_run_id: None,
            code_index,
            lsp_context,
            route_learning,
        }
    }

    /// Attach reusable run-record services. A fresh run and owner lease are
    /// created for every `run`/`run_stream` invocation.
    pub fn with_run_runtime(mut self, runtime: Arc<RunRuntime>) -> Self {
        self.run_runtime = Some(runtime);
        self
    }

    /// Bind this loop to the exact backend context that admitted its resolved
    /// execution mode. A mismatched context is rejected before any run record
    /// or effect can begin.
    pub fn with_execution_containment_context(
        mut self,
        context: Arc<ExecutionContainmentContext>,
    ) -> Result<Self, AgentError> {
        if context.mode() != self.config.execution_mode {
            return Err(AgentError::Config(
                "execution containment context does not match the resolved execution mode"
                    .to_string(),
            ));
        }
        self.tool_registry
            .set_execution_containment_context(Arc::clone(&context));
        self.execution_containment = Some(context);
        Ok(self)
    }

    pub fn with_run_context(mut self, context: RunInvocationContext) -> Self {
        self.run_context = context;
        self
    }

    pub fn required_run_runtime(&self) -> Result<Arc<RunRuntime>, AgentError> {
        self.run_runtime.clone().ok_or_else(|| {
            AgentError::Config(
                "agent execution requires an attached run-record runtime".to_string(),
            )
        })
    }

    pub(crate) fn with_parent_run_id(mut self, parent_run_id: RunId) -> Self {
        self.parent_run_id = Some(parent_run_id);
        self
    }

    fn init_code_index(config: &AgentConfig) -> Option<Arc<Mutex<CodeIndex>>> {
        if !config.code_index_enabled {
            return None;
        }
        let workspace_root = std::env::var("TERMINAL_CWD")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        if !workspace_root.exists() {
            return None;
        }
        let mut index = CodeIndex::default_for_workspace(workspace_root);
        let _ = index.ensure_fresh();
        Some(Arc::new(Mutex::new(index)))
    }

    fn build_lsp_context_config(config: &AgentConfig) -> LspContextConfig {
        let mut cfg = LspContextConfig::from_env();
        cfg.enabled = cfg.enabled && config.lsp_context_enabled;
        cfg.max_chars = config.lsp_context_max_chars.max(400);
        cfg
    }


    fn route_learning_effective_stats(
        stats: &RouteLearningStats,
        now_ms: i64,
    ) -> Option<RouteLearningStats> {
        if stats.samples == 0 {
            return None;
        }
        let mut out = stats.clone();
        if out.updated_at_unix_ms <= 0 {
            return Some(out);
        }
        let age_ms = now_ms.saturating_sub(out.updated_at_unix_ms).max(0);
        let ttl_secs = smart_routing_learning_ttl_secs();
        if ttl_secs > 0 {
            let ttl_ms = ttl_secs.saturating_mul(1000);
            if age_ms >= ttl_ms {
                return None;
            }
        }
        let half_life_secs = smart_routing_learning_half_life_secs();
        if half_life_secs <= 0 || age_ms <= 0 {
            return Some(out);
        }
        let half_life_ms = (half_life_secs.saturating_mul(1000)) as f64;
        let decay = (0.5_f64)
            .powf((age_ms as f64) / half_life_ms)
            .clamp(0.0, 1.0);
        let baseline_success = 0.90;
        let baseline_latency = 1800.0;
        out.success_rate = baseline_success + (out.success_rate - baseline_success) * decay;
        out.avg_latency_ms = baseline_latency + (out.avg_latency_ms - baseline_latency) * decay;
        out.consecutive_failures = ((out.consecutive_failures as f64) * decay).round() as u32;
        out.samples = ((out.samples as f64) * decay).round().max(1.0) as u32;
        Some(out)
    }

    fn prune_route_learning_locked(
        map: &mut HashMap<String, RouteLearningStats>,
        now_ms: i64,
    ) -> bool {
        let before = map.len();
        map.retain(|_, stats| Self::route_learning_effective_stats(stats, now_ms).is_some());
        map.len() != before
    }

    /// Attach an in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are actually executed by the orchestrator instead of just
    /// returning a signal envelope. See
    /// [`crate::sub_agent_orchestrator::SubAgentOrchestrator`].
    pub fn with_sub_agent_orchestrator(
        mut self,
        orchestrator: Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>,
    ) -> Self {
        self.sub_agent_orchestrator = Some(orchestrator);
        self
    }

    /// Attach the primary runtime credential pool (API key rotation).
    pub fn with_primary_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.primary_credential_pool = Some(pool);
        self
    }

    /// Attach a subject-bound memory manager and retain its immutable binding
    /// for internal provider helpers that must not inspect AgentConfig.
    pub fn with_memory(mut self, mm: Arc<std::sync::Mutex<MemoryManager>>) -> Self {
        if let Ok(manager) = mm.lock() {
            self.memory_subject = Some(manager.subject().clone());
            self.memory_subject_filter_key = Some(manager.subject_filter_key().clone());
        }
        let registry = Arc::make_mut(&mut self.tool_registry);
        if self.config.skip_memory {
            registry.disable_canonical_memory();
        } else if let Err(error) = registry.bind_canonical_memory_manager(Arc::clone(&mm)) {
            tracing::error!(
                error = %error,
                "canonical memory tool binding failed closed; memory tool disabled"
            );
            registry.disable_canonical_memory();
        }
        self.memory_manager = Some(mm);
        self
    }

    pub(crate) fn memory_subject_binding(
        &self,
    ) -> Option<(&MemorySubject, &MemorySubjectStorageKey)> {
        self.memory_subject
            .as_ref()
            .zip(self.memory_subject_filter_key.as_ref())
    }
    /// Export only the immutable subject attached when this loop received its
    /// memory manager. Callers cannot select a subject or audit actor.
    pub fn export_bound_memory(&self) -> Result<MemoryExportReport, MemoryManagerError> {
        self.with_bound_memory_manager(|manager, subject| {
            manager.export_subject(subject, manager.audit_actor())
        })
    }

    /// Return the verified export bytes through the exact descriptor validated
    /// by the bound manager. The caller can choose only a previously-issued
    /// opaque artifact descriptor; subject and audit actor stay immutable.
    pub fn open_bound_memory_export(
        &self,
        artifact: &MemoryExportArtifact,
    ) -> Result<std::fs::File, MemoryManagerError> {
        self.with_bound_memory_manager(|manager, subject| {
            manager.open_export_subject(subject, manager.audit_actor(), artifact)
        })
    }

    /// Forget only the immutable subject attached when this loop received its
    /// memory manager. A successful report has already passed provider
    /// readback and durable terminal-audit checks in [`MemoryManager`].
    pub fn forget_bound_memory(&self) -> Result<MemoryForgetReport, MemoryManagerError> {
        self.with_bound_memory_manager(|manager, subject| {
            manager.forget_subject(subject, manager.audit_actor())
        })
    }

    fn with_bound_memory_manager<T>(
        &self,
        operation: impl FnOnce(&MemoryManager, &MemorySubject) -> Result<T, MemoryManagerError>,
    ) -> Result<T, MemoryManagerError> {
        let (subject, filter) = self
            .memory_subject_binding()
            .ok_or(MemoryManagerError::SubjectState)?;
        let manager = self
            .memory_manager
            .as_ref()
            .ok_or(MemoryManagerError::SubjectState)?
            .lock()
            .map_err(|_| MemoryManagerError::SubjectState)?;
        if manager.subject() != subject || manager.subject_filter_key() != filter {
            return Err(MemoryManagerError::SubjectMismatch);
        }
        operation(&manager, subject)
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Drain observations emitted before this call while the caller's receiver is live.
    pub async fn flush_observer(&self) -> bool {
        match self.callbacks.observer_sink.as_ref() {
            Some(sink) => sink.flush().await,
            None => true,
        }
    }

    pub fn take_observer_dropped_event_count(&self) -> u64 {
        self.callbacks
            .observer_sink
            .as_ref()
            .map(AgentObserverSink::take_dropped_event_count)
            .unwrap_or(0)
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

}
