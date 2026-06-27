impl AgentLoop {
    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Remove duplicate tool calls that share the same function name and arguments.
    fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
        let mut seen = HashSet::new();
        let mut deduped = Vec::new();
        for tc in calls {
            let key = format!("{}:{}", tc.function.name, tc.function.arguments);
            if seen.insert(key) {
                deduped.push(tc.clone());
            } else {
                tracing::warn!("Deduplicated tool call: {}", tc.function.name);
            }
        }
        deduped
    }

    /// Try to repair an unknown tool name via case-insensitive or substring matching.
    /// Returns `true` if the tool call was repaired.
    fn repair_tool_call(&self, tc: &mut ToolCall) -> bool {
        if self.tool_registry.get(&tc.function.name).is_some() {
            return false;
        }
        let names = self.tool_registry.names();
        let target = tc.function.name.to_lowercase();

        if let Some(name) = names.iter().find(|n| n.to_lowercase() == target) {
            tracing::info!("Repaired tool call: '{}' → '{}'", tc.function.name, name);
            tc.function.name = name.clone();
            return true;
        }

        if let Some(name) = names
            .iter()
            .find(|n| n.to_lowercase().contains(&target) || target.contains(&n.to_lowercase()))
        {
            tracing::info!(
                "Repaired tool call (fuzzy): '{}' → '{}'",
                tc.function.name,
                name
            );
            tc.function.name = name.clone();
            return true;
        }
        false
    }

    /// Inject current session id into `session_search` calls when absent.
    fn hydrate_session_search_args(&self, tc: &mut ToolCall) {
        if tc.function.name != "session_search" {
            return;
        }
        let Some(session_id) = self.config.session_id.as_deref() else {
            return;
        };
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return;
        }

        let mut args: Value =
            serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
        let Some(obj) = args.as_object_mut() else {
            return;
        };
        let has_current = obj
            .get("current_session_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some();
        if has_current {
            return;
        }
        obj.insert(
            "current_session_id".to_string(),
            Value::String(session_id.to_string()),
        );
        if let Ok(updated) = serde_json::to_string(&args) {
            tc.function.arguments = updated;
        }
    }

    fn latest_user_text<'a>(&self, messages: &'a [Message]) -> Option<&'a str> {
        messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::User))
            .and_then(|m| m.content.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    fn primary_runtime_snapshot(&self) -> PrimaryRuntime {
        let provider = self
            .config
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let runtime_provider = provider
            .as_ref()
            .and_then(|p| self.config.runtime_providers.get(p))
            .or_else(|| {
                provider.as_ref().and_then(|p| {
                    self.config
                        .runtime_providers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(p))
                        .map(|(_, cfg)| cfg)
                })
            });
        let base_url = runtime_provider.and_then(|c| {
            c.base_url
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        let api_mode = runtime_provider
            .and_then(|c| c.api_mode.clone())
            .unwrap_or_else(|| self.config.api_mode.clone());
        let (command, args) = self.resolve_runtime_command_args(provider.as_deref());
        PrimaryRuntime {
            model: self.config.model.clone(),
            provider,
            base_url,
            api_mode,
            command,
            args,
            credential_pool: self.primary_credential_pool.clone(),
        }
    }

    fn turn_route_cost_guard(&self, model: String) -> TurnRuntimeRoute {
        let pri = self.primary_runtime_snapshot();
        let mut sig = pri.to_signature();
        sig.model = model.clone();
        TurnRuntimeRoute {
            model,
            provider: None,
            base_url: None,
            api_key_env: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            credential_pool: self.primary_credential_pool.clone(),
            credential_pool_fallback: true,
            route_label: None,
            routing_reason: Some("cost_guard".to_string()),
            signature: sig,
        }
    }

    fn turn_route_reliability_guard(&self, model: String) -> TurnRuntimeRoute {
        let pri = self.primary_runtime_snapshot();
        let mut sig = pri.to_signature();
        sig.model = model.clone();
        TurnRuntimeRoute {
            model,
            provider: None,
            base_url: None,
            api_key_env: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            credential_pool: self.primary_credential_pool.clone(),
            credential_pool_fallback: true,
            route_label: None,
            routing_reason: Some("reliability_guard".to_string()),
            signature: sig,
        }
    }

    fn try_build_cheap_runtime(
        &self,
        cheap: &CheapModelRouteConfig,
        explicit_api_key: Option<String>,
    ) -> Result<ResolvedCheapRuntime, ()> {
        let provider_raw = cheap.provider.as_deref().map(str::trim).unwrap_or("");
        if provider_raw.is_empty() {
            return Err(());
        }
        let provider_lc = provider_raw.to_lowercase();
        let model_full = cheap.model.as_deref().map(str::trim).unwrap_or("");
        if model_full.is_empty() {
            return Err(());
        }
        let (_, model_name) = self.extract_provider_and_model(model_full);
        let base_url = self.resolve_runtime_base_url(&provider_lc, cheap.base_url.as_deref());
        let api_mode = base_url
            .as_deref()
            .and_then(detect_api_mode_for_url)
            .unwrap_or(ApiMode::ChatCompletions);

        let has_runtime_override = explicit_api_key
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .is_some()
            || cheap
                .base_url
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
        let pool_ref = if has_runtime_override {
            None
        } else {
            self.primary_credential_pool.as_ref()
        };

        self.build_runtime_provider(
            &provider_lc,
            model_name,
            cheap.base_url.as_deref(),
            cheap.api_key_env.as_deref(),
            explicit_api_key.as_deref(),
            Some(&api_mode),
            pool_ref,
        )
        .map_err(|_| ())?;

        let (command, args) = self.resolve_runtime_command_args(Some(&provider_lc));
        if provider_lc == "copilot-acp"
            && command
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
            && !base_url
                .as_deref()
                .map(|u| u.starts_with("acp+tcp://"))
                .unwrap_or(false)
        {
            return Err(());
        }
        if provider_lc == "copilot-acp"
            && !base_url
                .as_deref()
                .map(|u| u.starts_with("acp+tcp://"))
                .unwrap_or(false)
        {
            if let Some(cmd) = command.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if which::which(cmd).is_err() {
                    return Err(());
                }
            }
        }
        Ok(ResolvedCheapRuntime {
            model: model_full.to_string(),
            provider: provider_lc,
            base_url,
            api_mode,
            command,
            args,
            credential_pool: if has_runtime_override {
                None
            } else {
                self.primary_credential_pool.clone()
            },
            skip_primary_credential_pool_fallback: has_runtime_override,
        })
    }

    fn route_learning_key(&self, provider_hint: Option<&str>, model: &str) -> String {
        let (inferred_provider, inferred_model) = self.extract_provider_and_model(model);
        let provider = provider_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(inferred_provider.as_str())
            .to_ascii_lowercase();
        format!(
            "{}:{}",
            provider,
            inferred_model.trim().to_ascii_lowercase()
        )
    }

    fn route_learning_key_for_route(
        &self,
        route: Option<&TurnRuntimeRoute>,
        response_model: Option<&str>,
    ) -> String {
        if let Some(model) = response_model.map(str::trim).filter(|s| !s.is_empty()) {
            let provider_hint = route.and_then(|r| r.provider.as_deref());
            return self.route_learning_key(provider_hint, model);
        }
        if let Some(rt) = route {
            return self.route_learning_key(rt.provider.as_deref(), rt.model.as_str());
        }
        self.route_learning_key(self.config.provider.as_deref(), self.config.model.as_str())
    }

    fn route_learning_stats_for_key(&self, key: &str) -> Option<RouteLearningStats> {
        let now_ms = now_unix_ms();
        let mut persist_snapshot: Option<HashMap<String, RouteLearningStats>> = None;
        let stats = if let Ok(mut map) = self.route_learning.lock() {
            let mut changed = Self::prune_route_learning_locked(&mut map, now_ms);
            let out = map
                .get(key)
                .and_then(|stats| Self::route_learning_effective_stats(stats, now_ms));
            if out.is_none() && map.remove(key).is_some() {
                changed = true;
            }
            if changed {
                persist_snapshot = Some(map.clone());
            }
            out
        } else {
            None
        };
        if let Some(snapshot) = persist_snapshot {
            self.save_route_learning_state(&snapshot);
        }
        stats
    }

    fn route_learning_score(stats: Option<&RouteLearningStats>, cheap_bias: f64) -> f64 {
        let success_rate = stats.map(|s| s.success_rate).unwrap_or(0.90);
        let avg_latency_ms = stats.map(|s| s.avg_latency_ms).unwrap_or(1800.0);
        let latency_score = (1.0 / (1.0 + (avg_latency_ms / 2500.0))).clamp(0.05, 1.0);
        let failure_penalty = stats
            .map(|s| (s.consecutive_failures as f64 * 0.08).min(0.35))
            .unwrap_or(0.0);
        let exploration_bonus = stats
            .map(|s| {
                let coverage = (s.samples.min(20) as f64) / 20.0;
                (1.0 - coverage) * 0.03
            })
            .unwrap_or(0.03);
        (success_rate * 0.60) + (latency_score * 0.30) + cheap_bias + exploration_bonus
            - failure_penalty
    }

    fn update_route_learning(
        &self,
        route: Option<&TurnRuntimeRoute>,
        response_model: Option<&str>,
        latency_ms: u64,
        success: bool,
    ) {
        if !smart_routing_learning_enabled() {
            return;
        }
        let key = self.route_learning_key_for_route(route, response_model);
        let alpha = smart_routing_learning_alpha();
        let mut persist_snapshot: Option<HashMap<String, RouteLearningStats>> = None;
        if let Ok(mut map) = self.route_learning.lock() {
            let now_ms = now_unix_ms();
            let _ = Self::prune_route_learning_locked(&mut map, now_ms);
            let entry = map.entry(key).or_insert_with(RouteLearningStats::default);
            entry.samples = entry.samples.saturating_add(1);
            if entry.samples == 1 {
                entry.success_rate = if success { 1.0 } else { 0.0 };
                entry.avg_latency_ms = latency_ms as f64;
            } else {
                let observed_success = if success { 1.0 } else { 0.0 };
                entry.success_rate = (1.0 - alpha) * entry.success_rate + alpha * observed_success;
                entry.avg_latency_ms =
                    (1.0 - alpha) * entry.avg_latency_ms + alpha * (latency_ms as f64);
            }
            if success {
                entry.consecutive_failures = 0;
            } else {
                entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
            }
            entry.updated_at_unix_ms = now_ms;
            persist_snapshot = Some(map.clone());
        }
        if let Some(snapshot) = persist_snapshot {
            self.save_route_learning_state(&snapshot);
        }
    }

    fn route_learning_snapshot(
        &self,
        route: Option<&TurnRuntimeRoute>,
        response_model: Option<&str>,
    ) -> Value {
        let key = self.route_learning_key_for_route(route, response_model);
        let stats = self.route_learning_stats_for_key(&key);
        let score = Self::route_learning_score(stats.as_ref(), 0.0);
        let ttl_secs = smart_routing_learning_ttl_secs();
        let half_life_secs = smart_routing_learning_half_life_secs();
        serde_json::json!({
            "key": key,
            "enabled": smart_routing_learning_enabled(),
            "ttl_secs": ttl_secs,
            "half_life_secs": half_life_secs,
            "score": score,
            "stats": stats,
        })
    }

    fn resolve_smart_runtime_route(&self, messages: &[Message]) -> Option<TurnRuntimeRoute> {
        let text = self.latest_user_text(messages)?;
        let primary = self.primary_runtime_snapshot();
        let outcome = resolve_turn_route(
            text,
            &self.config.smart_model_routing,
            &primary,
            |cheap, explicit_key| self.try_build_cheap_runtime(cheap, explicit_key),
        );

        match outcome {
            ResolveTurnOutcome::CheapRouted {
                model,
                label,
                runtime,
                signature,
            } => {
                let primary_key =
                    self.route_learning_key(primary.provider.as_deref(), primary.model.as_str());
                let cheap_key = self
                    .route_learning_key(Some(runtime.provider.as_str()), runtime.model.as_str());
                let primary_stats = self.route_learning_stats_for_key(&primary_key);
                let cheap_stats = self.route_learning_stats_for_key(&cheap_key);
                let primary_score = Self::route_learning_score(primary_stats.as_ref(), 0.0);
                let cheap_score = Self::route_learning_score(
                    cheap_stats.as_ref(),
                    smart_routing_learning_cheap_bias(),
                );
                let margin = smart_routing_learning_switch_margin();
                if smart_routing_learning_enabled() && (cheap_score + margin) < primary_score {
                    tracing::debug!(
                        primary_key = %primary_key,
                        cheap_key = %cheap_key,
                        primary_score,
                        cheap_score,
                        margin,
                        "smart routing online-learning selected primary route"
                    );
                    return None;
                }
                let cheap = self.config.smart_model_routing.cheap_model.as_ref()?;
                Some(TurnRuntimeRoute {
                    model,
                    provider: Some(runtime.provider.clone()),
                    base_url: runtime.base_url.clone(),
                    api_key_env: cheap
                        .api_key_env
                        .as_ref()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    api_mode: Some(runtime.api_mode.clone()),
                    command: runtime.command.clone(),
                    args: runtime.args.clone(),
                    credential_pool: runtime.credential_pool.clone(),
                    credential_pool_fallback: !runtime.skip_primary_credential_pool_fallback,
                    route_label: Some(format!(
                        "{} [cheap_score={:.3} primary_score={:.3}]",
                        label, cheap_score, primary_score
                    )),
                    routing_reason: Some("simple_turn_online_learning".to_string()),
                    signature,
                })
            }
            ResolveTurnOutcome::Primary { .. } => None,
        }
    }

    /// Resolve the model used for automatic degradation when nearing
    /// `max_cost_usd`.
    fn resolve_cost_degrade_model(&self) -> Option<String> {
        if let Some(ref m) = self.config.cost_guard_degrade_model {
            if !m.trim().is_empty() {
                return Some(m.trim().to_string());
            }
        }
        if let Some(ref m) = self.config.retry.fallback_model {
            if !m.trim().is_empty() {
                return Some(m.trim().to_string());
            }
        }
        if self.config.model.trim() != "openai:gpt-4o-mini" {
            return Some("openai:gpt-4o-mini".to_string());
        }
        None
    }

    fn resolve_reliability_degrade_model(
        &self,
        active_model: &str,
        route: Option<&TurnRuntimeRoute>,
    ) -> Option<String> {
        if let Some(ref fallback) = self.config.retry.fallback_model {
            if !fallback.trim().is_empty() && !fallback.eq_ignore_ascii_case(active_model) {
                return Some(fallback.trim().to_string());
            }
        }
        let provider_hint = route
            .and_then(|r| r.provider.as_deref())
            .or(self.config.provider.as_deref())
            .unwrap_or("openai");
        let (_, active_model_id) = self.extract_provider_and_model(active_model);
        if let Some(candidate) =
            preferred_tool_payload_fallback_model(provider_hint, active_model_id)
        {
            let normalized = if candidate.contains(':') {
                candidate
            } else {
                format!("{}:{}", provider_hint, candidate)
            };
            if !normalized.eq_ignore_ascii_case(active_model) {
                return Some(normalized);
            }
        }
        if !active_model.eq_ignore_ascii_case("openai:gpt-4o-mini") {
            return Some("openai:gpt-4o-mini".to_string());
        }
        None
    }

    fn resolve_retry_failover_chain(&self, active_model: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let active_lc = active_model.trim().to_ascii_lowercase();

        let mut push_candidate = |candidate: &str| {
            let trimmed = candidate.trim();
            if trimmed.is_empty() {
                return;
            }
            let normalized = trimmed.to_ascii_lowercase();
            if normalized == active_lc {
                return;
            }
            if seen.insert(normalized) {
                out.push(trimmed.to_string());
            }
        };

        for model in &self.config.retry.fallback_models {
            push_candidate(model);
        }
        if let Some(ref fallback) = self.config.retry.fallback_model {
            push_candidate(fallback);
        }
        if let Some(dynamic) = self.resolve_reliability_degrade_model(active_model, None) {
            push_candidate(&dynamic);
        }

        out
    }

    /// Ask the LLM for a final summary when the turn budget is exhausted.
    async fn handle_max_iterations(
        &self,
        ctx: &mut ContextManager,
    ) -> Result<Option<Message>, AgentError> {
        ctx.add_message(Message::system(
            "[SYSTEM] Maximum conversation turns reached. Please provide a brief summary of \
             what was accomplished and any remaining tasks.",
        ));
        let (_, model_name) = self.extract_provider_and_model(self.config.model.as_str());
        let response = self
            .llm_provider
            .chat_completion(
                ctx.get_messages(),
                &[],
                self.config.max_tokens,
                self.config.temperature,
                Some(model_name),
                self.extra_body_for_api_mode(&self.config.api_mode).as_ref(),
            )
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        Ok(Some(response.message))
    }

    async fn handle_tool_loop_guard_summary(
        &self,
        ctx: &mut ContextManager,
        consecutive_error_turns: u32,
        failed_calls: u32,
        total_calls: usize,
    ) -> Result<Option<Message>, AgentError> {
        ctx.add_message(Message::system(format!(
            "[SYSTEM] Tool-loop guard triggered after {} consecutive error turn(s). Latest turn failed {}/{} tool call(s). Stop calling tools and provide a concise final response with what succeeded, what failed, and precise next manual step(s).",
            consecutive_error_turns, failed_calls, total_calls
        )));
        let (_, model_name) = self.extract_provider_and_model(self.config.model.as_str());
        let response = self
            .llm_provider
            .chat_completion(
                ctx.get_messages(),
                &[],
                self.config.max_tokens,
                self.config.temperature,
                Some(model_name),
                self.extra_body_for_api_mode(&self.config.api_mode).as_ref(),
            )
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        Ok(Some(response.message))
    }

    fn execute_tool_call_terminal(
        registry: &ToolRegistry,
        tool_call_id: &str,
        tool_name: &str,
        mut params: Value,
        max_delegate_depth: u32,
        current_delegate_depth: u32,
        parent_budget_remaining_usd: Option<f64>,
    ) -> ToolResult {
        match registry.get(tool_name) {
            Some(entry) => {
                if tool_name == "delegate_task" {
                    if current_delegate_depth >= max_delegate_depth {
                        return ToolResult::err(
                            tool_call_id,
                            format!(
                                "Delegation depth limit reached ({}/{}).",
                                current_delegate_depth, max_delegate_depth
                            ),
                        );
                    }
                    if let Some(obj) = params.as_object_mut() {
                        obj.insert(
                            "child_depth".to_string(),
                            Value::from(current_delegate_depth + 1),
                        );
                        obj.insert("max_depth".to_string(), Value::from(max_delegate_depth));
                        if let Some(remaining) = parent_budget_remaining_usd {
                            obj.insert(
                                "parent_budget_remaining_usd".to_string(),
                                Value::from(remaining),
                            );
                        }
                    }
                }

                match (entry.handler)(params) {
                    Ok(output) => {
                        if looks_like_tool_error_output(&output) {
                            ToolResult::err(tool_call_id, output)
                        } else {
                            ToolResult::ok(tool_call_id, output)
                        }
                    }
                    Err(e) => ToolResult::err(tool_call_id, e.to_string()),
                }
            }
            None => {
                let available = registry.names().join(", ");
                let error_msg = format!(
                    "Unknown tool '{}'. Available tools: [{}]",
                    tool_name, available
                );
                ToolResult::err(tool_call_id, error_msg)
            }
        }
    }

    /// Execute a batch of tool calls in parallel using a JoinSet.
    async fn execute_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn: u32,
        tool_concurrency: usize,
        tool_schemas: &[ToolSchema],
        contextlattice_connect_intent: bool,
        parent_budget_remaining_usd: Option<f64>,
        tool_errors: &mut Vec<hermes_core::ToolErrorRecord>,
    ) -> Vec<ToolResult> {
        let mut join_set = JoinSet::new();
        let tool_concurrency = tool_concurrency.max(1);
        let mut results = Vec::with_capacity(tool_calls.len());
        let max_delegate_depth = self.resolve_max_delegate_depth();
        let current_delegate_depth = self.delegate_depth;
        let orchestrator = self.sub_agent_orchestrator.clone();

        // Run orchestrated `delegate_task` calls sequentially in the caller's
        // task — this keeps the inner AgentLoop future out of the Send-bound
        // JoinSet and preserves the requested concurrency cap which is already
        // applied upstream via `cap_delegates`.
        let mut orchestrated: Vec<ToolResult> = Vec::new();
        if let Some(orch) = orchestrator.as_ref() {
            for tc in tool_calls {
                if tc.function.name != "delegate_task" {
                    continue;
                }
                if current_delegate_depth >= max_delegate_depth {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        format!(
                            "Delegation depth limit reached ({}/{}).",
                            current_delegate_depth, max_delegate_depth
                        ),
                    ));
                    continue;
                }
                let parsed: Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        orchestrated.push(ToolResult::err(
                            &tc.id,
                            format!(
                                "Invalid JSON params for tool 'delegate_task': {}. \
                                 Please retry with valid JSON.",
                                e
                            ),
                        ));
                        continue;
                    }
                };
                let task = parsed
                    .get("task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if task.trim().is_empty() {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        "delegate_task requires non-empty 'task' string.",
                    ));
                    continue;
                }
                let requested_toolset = parsed
                    .get("toolset")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let background = parsed
                    .get("background")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let req = crate::sub_agent_orchestrator::SubAgentRequest {
                    task,
                    context: parsed
                        .get("context")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    inherited_tool_schemas: if requested_toolset.is_none() {
                        tool_schemas.to_vec()
                    } else {
                        Vec::new()
                    },
                    toolset: requested_toolset,
                    model: parsed
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    child_depth: current_delegate_depth + 1,
                    max_depth: max_delegate_depth,
                    parent_budget_remaining_usd,
                    background,
                };
                // Orchestrator internally runs the child on its own
                // `tokio::spawn` task, which erases the child future and breaks
                // async recursion between parent / child `execute_tool_calls`.
                let output = orch.execute(req).await;
                orchestrated.push(ToolResult::ok(&tc.id, output));
            }
        }

        for tc in tool_calls {
            // Skip `delegate_task` when an orchestrator already handled it.
            if orchestrator.is_some() && tc.function.name == "delegate_task" {
                continue;
            }
            if contextlattice_connect_intent
                && tc.function.name == "terminal"
                && is_contextlattice_shell_invocation(&tc.function.arguments)
            {
                let msg = "ContextLattice integration requests must use `contextlattice_search` / `contextlattice_context_pack`, not shell command `contextlattice`. Retry by calling `contextlattice_search` first with a scoped query.".to_string();
                tool_errors.push(hermes_core::ToolErrorRecord {
                    tool_name: tc.function.name.clone(),
                    error: msg.clone(),
                    turn,
                });
                results.push(ToolResult::err(&tc.id, msg));
                continue;
            }
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();
            let plugin_manager = self.plugin_manager.clone();
            let max_delegate_depth = max_delegate_depth;
            let current_delegate_depth = current_delegate_depth;
            let parent_budget_remaining_usd = parent_budget_remaining_usd;

            join_set.spawn(async move {
                let params: Value = match serde_json::from_str(&raw_args) {
                    Ok(v) => v,
                    Err(e) => {
                        let error_msg = format!(
                            "Invalid JSON params for tool '{}': {}. \
                             Please check your parameters and retry with valid JSON.",
                            tool_name, e
                        );
                        return ToolResult::err(&tool_call_id, error_msg);
                    }
                };
                let middleware_ctx = ToolExecutionMiddlewareContext {
                    tool_name: tool_name.clone(),
                    tool_call_id: tool_call_id.clone(),
                    args: params.clone(),
                    original_args: params.clone(),
                    turn,
                };
                let terminal = |next_params: Value| {
                    Self::execute_tool_call_terminal(
                        &registry,
                        &tool_call_id,
                        &tool_name,
                        next_params,
                        max_delegate_depth,
                        current_delegate_depth,
                        parent_budget_remaining_usd,
                    )
                };
                if let Some(pm) = plugin_manager {
                    match pm.lock() {
                        Ok(pm) => pm.run_tool_execution_middleware(middleware_ctx, terminal),
                        Err(_) => {
                            tracing::warn!(
                                "Plugin manager lock poisoned while running tool execution middleware"
                            );
                            terminal(params)
                        }
                    }
                } else {
                    terminal(params)
                }
            });
            if join_set.len() >= tool_concurrency {
                if let Some(result) = join_set.join_next().await {
                    match result {
                        Ok(tool_result) => {
                            if tool_result.is_error {
                                let tc = tool_calls
                                    .iter()
                                    .find(|tc| tc.id == tool_result.tool_call_id);
                                if let Some(tc) = tc {
                                    tool_errors.push(hermes_core::ToolErrorRecord {
                                        tool_name: tc.function.name.clone(),
                                        error: tool_result.content.clone(),
                                        turn,
                                    });
                                }
                            }
                            results.push(tool_result);
                        }
                        Err(e) => {
                            tracing::error!("Task join error: {}", e);
                        }
                    }
                }
            }
        }

        for tool_result in orchestrated {
            if tool_result.is_error {
                let tc = tool_calls
                    .iter()
                    .find(|tc| tc.id == tool_result.tool_call_id);
                if let Some(tc) = tc {
                    tool_errors.push(hermes_core::ToolErrorRecord {
                        tool_name: tc.function.name.clone(),
                        error: tool_result.content.clone(),
                        turn,
                    });
                }
            }
            results.push(tool_result);
        }
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(tool_result) => {
                    if tool_result.is_error {
                        // Record the error but we still add the result to context
                        let tc = tool_calls
                            .iter()
                            .find(|tc| tc.id == tool_result.tool_call_id);
                        if let Some(tc) = tc {
                            tool_errors.push(hermes_core::ToolErrorRecord {
                                tool_name: tc.function.name.clone(),
                                error: tool_result.content.clone(),
                                turn,
                            });
                        }
                    }
                    results.push(tool_result);
                }
                Err(e) => {
                    tracing::error!("Task join error: {}", e);
                }
            }
        }

        results
    }

    fn resolve_max_delegate_depth(&self) -> u32 {
        std::env::var("HERMES_MAX_DELEGATE_DEPTH")
            .ok()
            .and_then(|v| parse_delegate_depth(&v))
            .unwrap_or_else(|| normalize_delegate_depth(self.config.max_delegate_depth))
    }

    /// Cap concurrent delegate_task calls based on config.
    fn cap_delegates(&self, tool_calls: &mut Vec<ToolCall>) {
        if delegation_spawning_paused() {
            let delegate_count = tool_calls
                .iter()
                .filter(|tc| tc.function.name == "delegate_task")
                .count();
            if delegate_count > 0 {
                tracing::warn!(
                    "Dropping {} delegate_task call(s): delegation spawning is paused",
                    delegate_count
                );
                tool_calls.retain(|tc| tc.function.name != "delegate_task");
            }
            return;
        }
        let delegate_count = tool_calls
            .iter()
            .filter(|tc| tc.function.name == "delegate_task")
            .count() as u32;
        if delegate_count > self.config.max_concurrent_delegates {
            tracing::warn!(
                "Capping delegate_task calls from {} to {}",
                delegate_count,
                self.config.max_concurrent_delegates
            );
            let mut kept_delegates = 0u32;
            tool_calls.retain(|tc| {
                if tc.function.name == "delegate_task" {
                    if kept_delegates < self.config.max_concurrent_delegates {
                        kept_delegates += 1;
                        true
                    } else {
                        false
                    }
                } else {
                    true
                }
            });
        }
    }

    fn emit_background_review_metrics(&self, turn: u32, ctx: &ContextManager) {
        if !self.config.background_review_metrics_enabled {
            return;
        }
        let snapshot = ctx.get_messages().to_vec();
        tokio::spawn(async move {
            let tool_msg_count = snapshot
                .iter()
                .filter(|m| matches!(m.role, hermes_core::MessageRole::Tool))
                .count();
            tracing::debug!(
                turn,
                tool_messages = tool_msg_count,
                total_messages = snapshot.len(),
                "Background review snapshot captured"
            );
        });
    }

    /// Metrics (always) + optional Python-style memory/skill review LLM pass on session end.
    fn spawn_background_review(&self, turn: u32, ctx: &ContextManager, review_memory_at_end: bool) {
        self.emit_background_review_metrics(turn, ctx);
        if !self.config.background_review_enabled {
            return;
        }
        let mut review_skills = false;
        if self.config.skill_creation_nudge_interval > 0
            && self
                .tool_registry
                .names()
                .iter()
                .any(|n| n == "skill_manage")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                if c.iters_since_skill >= self.config.skill_creation_nudge_interval {
                    review_skills = true;
                    c.iters_since_skill = 0;
                }
            }
        }
        let review_memory = review_memory_at_end;
        if !review_memory && !review_skills {
            return;
        }
        let prompt: &'static str = match (review_memory, review_skills) {
            (true, true) => COMBINED_REVIEW_PROMPT,
            (true, false) => MEMORY_REVIEW_PROMPT,
            (false, true) => SKILL_REVIEW_PROMPT,
            _ => return,
        };
        let mut hist = ctx.get_messages().to_vec();
        hist.push(Message::user(prompt));
        let mut cfg = self.config.clone();
        cfg.background_review_enabled = false;
        cfg.background_review_metrics_enabled = false;
        cfg.memory_nudge_interval = 0;
        cfg.skill_creation_nudge_interval = 0;
        cfg.max_concurrent_delegates = 0;
        cfg.quiet_mode = true;
        cfg.max_turns = if cfg.max_turns == 0 {
            16
        } else {
            cfg.max_turns.min(16)
        };
        let tools = self.tool_registry.clone();
        let provider = self.llm_provider.clone();
        let review_cb = self.callbacks.background_review_callback.clone();
        tokio::spawn(async move {
            let agent = AgentLoop::new(cfg, tools, provider);
            match agent.run(hist, None).await {
                Ok(result) => {
                    if let Some(cb) = review_cb.as_ref() {
                        if let Some(summary) = summarize_background_review_result(&result.messages)
                        {
                            cb(&summary);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "background memory/skill review failed");
                }
            }
        });
    }

    /// Recover todo-state hints from historical messages at loop start.
    fn hydrate_todo_store(&self, ctx: &ContextManager) {
        let todo_markers = ctx
            .get_messages()
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| c.contains("TODO") || c.contains("[ ]") || c.contains("[x]"))
            .count();
        if todo_markers > 0 {
            tracing::debug!(todo_markers, "Hydrated todo markers from prior context");
        }
    }
}
