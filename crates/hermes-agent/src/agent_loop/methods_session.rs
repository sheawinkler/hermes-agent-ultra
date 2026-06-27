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
        let route_learning = Arc::new(Mutex::new(Self::load_route_learning_state(&config)));
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
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
        let route_learning = Arc::new(Mutex::new(Self::load_route_learning_state(&config)));
        let code_index = Self::init_code_index(&config);
        let lsp_context = Self::build_lsp_context_config(&config);
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
            memory_manager: None,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh_backoff: Arc::new(Mutex::new(HashMap::new())),
            sub_agent_orchestrator: None,
            code_index,
            lsp_context,
            route_learning,
        }
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

    fn load_route_learning_state(config: &AgentConfig) -> HashMap<String, RouteLearningStats> {
        if !smart_routing_learning_enabled() {
            return HashMap::new();
        }
        let path = route_learning_state_path(config);
        let raw = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => return HashMap::new(),
        };
        let parsed: RouteLearningState = match serde_json::from_str(&raw) {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "failed to parse route-learning state; starting empty"
                );
                return HashMap::new();
            }
        };
        let mut entries = parsed.entries;
        let now_ms = now_unix_ms();
        let _ = Self::prune_route_learning_locked(&mut entries, now_ms);
        entries
    }

    fn save_route_learning_state(&self, entries: &HashMap<String, RouteLearningStats>) {
        let path = route_learning_state_path(&self.config);
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    path = %parent.display(),
                    error = %err,
                    "failed to create route-learning state directory"
                );
                return;
            }
        }
        let body = RouteLearningState {
            schema_version: 1,
            saved_at_unix_ms: now_unix_ms(),
            entries: entries.clone(),
        };
        let serialized = match serde_json::to_vec_pretty(&body) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(error = %err, "failed to serialize route-learning state");
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(err) = std::fs::write(&tmp, serialized) {
            tracing::warn!(
                path = %tmp.display(),
                error = %err,
                "failed to write route-learning state temp file"
            );
            return;
        }
        if let Err(err) = std::fs::rename(&tmp, &path) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "failed to move route-learning state into place"
            );
        }
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

    /// Set the memory manager.
    pub fn with_memory(mut self, mm: Arc<std::sync::Mutex<MemoryManager>>) -> Self {
        self.memory_manager = Some(mm);
        self
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

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

}
