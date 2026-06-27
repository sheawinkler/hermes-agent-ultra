impl AgentLoop {
    fn credential_pool_for_route<'a>(
        &'a self,
        rt: &'a TurnRuntimeRoute,
    ) -> Option<&'a Arc<CredentialPool>> {
        if rt.credential_pool_fallback {
            rt.credential_pool
                .as_ref()
                .or(self.primary_credential_pool.as_ref())
        } else {
            rt.credential_pool.as_ref()
        }
    }

    fn messages_for_api_call(&self, ctx: &ContextManager) -> Vec<Message> {
        let mut messages = ctx.get_messages().to_vec();
        if let Some(ephemeral) = self
            .config
            .ephemeral_system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            messages.push(Message::system(ephemeral));
        }
        self.apply_prompt_cache_markers(&mut messages);
        messages
    }

    fn apply_prompt_cache_markers(&self, messages: &mut Vec<Message>) {
        use hermes_core::types::{CacheControl, CacheType, MessageRole};
        if messages.is_empty() {
            return;
        }
        let provider = self
            .config
            .provider
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_anthropic = provider.contains("anthropic") || provider.contains("claude");
        if !is_anthropic {
            return;
        }
        for msg in messages.iter_mut() {
            if msg.role == MessageRole::System {
                msg.cache_control = Some(CacheControl {
                    cache_type: CacheType::Persistent,
                });
                break;
            }
        }
        let ephemeral_budget = 4;
        let mut marked = 0;
        for msg in messages.iter_mut().rev() {
            if marked >= ephemeral_budget {
                break;
            }
            if msg.role == MessageRole::User || msg.role == MessageRole::Assistant {
                msg.cache_control = Some(CacheControl {
                    cache_type: CacheType::Ephemeral,
                });
                marked += 1;
            }
        }
    }

    /// Build the full system prompt including identity, memory, and plugin context.
    ///
    /// Aligns with Python behavior:
    /// - prefer `~/.hermes/SOUL.md` as identity
    /// - fallback to `DEFAULT_AGENT_IDENTITY`
    /// - then append optional configured `system_prompt`
    fn build_system_prompt(
        &self,
        _task_hint: &str,
        tool_schemas: &[ToolSchema],
        model_for_prompt: &str,
    ) -> String {
        let soul = load_soul_md_from_home(self.config.hermes_home.as_deref());
        let mut builder = SystemPromptBuilder::new().with_personality(soul.as_deref());
        if let Some(base) = self.config.system_prompt.as_deref() {
            builder = builder.with_system_message(base);
        }
        builder = builder.with_block(CONVERSATIONAL_SUPPORT_GUIDANCE);
        let tool_names: HashSet<&str> = tool_schemas.iter().map(|t| t.name.as_str()).collect();
        let mut tool_guidance = Vec::new();
        if tool_names.contains("memory") {
            tool_guidance.push(MEMORY_GUIDANCE);
        }
        if tool_names.contains("session_search") {
            tool_guidance.push(SESSION_SEARCH_GUIDANCE);
        }
        if tool_names.contains("skill_manage") {
            tool_guidance.push(SKILLS_GUIDANCE);
        }
        if !tool_guidance.is_empty() {
            builder = builder.with_tool_guidance(&tool_guidance.join(" "));
        }

        if !tool_names.is_empty() {
            builder = builder.with_block(STEER_CHANNEL_NOTE);
        }

        if !tool_names.is_empty() && self.should_inject_tool_enforcement(model_for_prompt) {
            builder = builder.with_block(TOOL_USE_ENFORCEMENT_GUIDANCE);
            let model_lower = model_for_prompt.to_lowercase();
            if model_lower.contains("gemini") || model_lower.contains("gemma") {
                builder = builder.with_block(GOOGLE_MODEL_OPERATIONAL_GUIDANCE);
            }
            if model_lower.contains("gpt") || model_lower.contains("codex") {
                builder = builder.with_block(OPENAI_MODEL_EXECUTION_GUIDANCE);
            }
        }
        if tool_names.contains("contextlattice_search")
            || tool_names.contains("contextlattice_context_pack")
        {
            builder = builder.with_block(CONTEXTLATTICE_OPERATIONAL_GUIDANCE);
        }
        let runtime_mode = resolve_runtime_mode(
            self.config.platform.as_deref(),
            None,
            Some(&self.config.coding_context),
            Some(model_for_prompt),
        );
        let has_coding_tools = [
            "terminal",
            "read_file",
            "write_file",
            "patch",
            "search_files",
        ]
        .iter()
        .any(|tool| tool_names.contains(tool));
        let hidden_skill_categories = if has_coding_tools {
            runtime_mode.hidden_skill_categories()
        } else {
            &[]
        };
        if has_coding_tools {
            for block in runtime_mode.system_blocks() {
                builder = builder.with_block(&block);
            }
        }

        if let Some(ref personality) = self.config.personality {
            let requested = personality.trim();
            if !requested.is_empty() {
                if requested.eq_ignore_ascii_case("default") {
                    // "default" means keep SOUL/default identity only.
                } else if let Some(profile) =
                    resolve_personality(requested, self.config.hermes_home.as_deref())
                {
                    builder = builder
                        .with_block(&format!("## Active Personality ({requested})\n{profile}"));
                } else if requested.contains(char::is_whitespace) {
                    // Compatibility path: historically this field was appended verbatim.
                    builder = builder.with_block(&format!("Personality: {requested}"));
                    tracing::warn!(
                        "personality '{requested}' not found as a named profile; using inline value"
                    );
                } else {
                    tracing::warn!(
                        "personality '{}' not found; falling back to default identity",
                        requested
                    );
                }
            }
        }

        if !self.config.skip_memory {
            let (memory_block, user_block) =
                load_builtin_memory_snapshot(self.config.hermes_home.as_deref());
            if let Some(block) = memory_block {
                builder = builder.with_block(&block);
            }
            if let Some(block) = user_block {
                builder = builder.with_block(&block);
            }
        }

        let mem_block = self.memory_system_prompt();
        if !mem_block.is_empty() {
            builder = builder.with_memory_context(&mem_block);
        }

        if let Some(skills_prompt) = self.skills_system_prompt(&tool_names, hidden_skill_categories)
        {
            builder = builder.with_skills_prompt(&skills_prompt);
        }

        if let Some(context_prompt) = self.context_files_prompt() {
            builder = builder.with_context_files(&context_prompt);
        }
        if let Some(repo_map) = self.code_index_repo_map_block() {
            builder = builder.with_block(&repo_map);
        }

        let provider = self.effective_provider_for_prompt(model_for_prompt);
        builder = builder.with_timestamp(Some(model_for_prompt), provider.as_deref());

        let mut timestamp_extras = String::new();
        if self.config.pass_session_id {
            if let Some(ref sid) = self.config.session_id {
                if !sid.trim().is_empty() {
                    timestamp_extras.push_str(&format!("Session ID: {}\n", sid.trim()));
                }
            }
        }
        if !timestamp_extras.trim().is_empty() {
            builder = builder.with_block(timestamp_extras.trim_end());
        }

        if provider.as_deref() == Some("alibaba") {
            let model_short = model_for_prompt
                .split('/')
                .next_back()
                .unwrap_or(model_for_prompt);
            builder = builder.with_block(&format!(
                "You are powered by the model named {}. The exact model ID is {}. When asked what model you are, always answer based on this information, not on any model name returned by the API.",
                model_short, model_for_prompt
            ));
        }

        if let Some(hint) = self.platform_hint_text() {
            builder = builder.with_block(hint);
        }

        builder.build().to_string()
    }

    /// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
    fn resolve_initial_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        if let Some(ref s) = self.config.stored_system_prompt {
            let t = s.trim();
            if !t.is_empty() {
                return (s.clone(), true);
            }
        }
        (
            self.build_system_prompt(task_hint, tool_schemas, &self.config.model),
            false,
        )
    }

    /// Compress when total chars exceed 80% of the context budget (Python auto-compaction).
    fn auto_compress_if_over_threshold(&self, ctx: &mut ContextManager) {
        let total_chars = ctx.total_chars();
        let max_c = ctx.max_context_chars().max(1);
        let threshold = (max_c as f64 * 0.8) as usize;
        if total_chars <= threshold {
            return;
        }
        let message = format!(
            "Context pressure at {}%, triggering compression",
            (total_chars * 100) / max_c
        );
        tracing::info!("{message}");
        self.emit_status("lifecycle", &message);
        if let Some(note) = self.memory_pre_compress_note(ctx.get_messages()) {
            ctx.add_message(Message::system(note));
        }
        ctx.compress();
        let after_chars = ctx.total_chars();
        self.emit_compaction_contextlattice_checkpoint(total_chars, after_chars, max_c);
    }

    fn emit_compaction_contextlattice_checkpoint(
        &self,
        before_chars: usize,
        after_chars: usize,
        max_context_chars: usize,
    ) {
        let mode = compaction_governance_mode_runtime();
        if matches!(mode, CompactionGovernanceMode::Off) {
            return;
        }

        let session = self
            .config
            .session_id
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("session");
        let topic = format!("runbooks/alpha/compaction/{}", session);
        let pressure_before = ((before_chars as f64 / max_context_chars as f64) * 100.0).round();
        let pressure_after = ((after_chars as f64 / max_context_chars as f64) * 100.0).round();
        let content = format!(
            "compaction_event mode={} session={} before_chars={} after_chars={} max_chars={} pressure_before_pct={} pressure_after_pct={}",
            mode.as_str(),
            session,
            before_chars,
            after_chars,
            max_context_chars,
            pressure_before,
            pressure_after
        );

        match write_contextlattice_checkpoint(&topic, CONTEXTLATTICE_COMPACTION_FILE, &content) {
            Ok(()) => {
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "ContextLattice compaction checkpoint written ({}% -> {}%).",
                        pressure_before, pressure_after
                    ),
                );
            }
            Err(err) => {
                if matches!(mode, CompactionGovernanceMode::Enforce) {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Compaction governance enforce-mode: checkpoint failed: {}",
                            err
                        ),
                    );
                }
            }
        }
    }

    /// Emit explicit preflight compression status before first LLM call.
    fn preflight_context_compress_with_status(&self, ctx: &mut ContextManager) {
        let max_c = ctx.max_context_chars().max(1);
        let before = ctx.total_chars();
        let threshold = (max_c as f64 * 0.8) as usize;
        let before_pct = (before * 100) / max_c;
        if before <= threshold {
            self.emit_status(
                "lifecycle",
                &format!(
                    "Preflight compression check: {}% context usage, no compression needed",
                    before_pct
                ),
            );
            return;
        }
        self.emit_status(
            "lifecycle",
            &format!(
                "Preflight compression check: {}% context usage, compressing before first turn",
                before_pct
            ),
        );
        self.auto_compress_if_over_threshold(ctx);
        let after = ctx.total_chars();
        let after_pct = (after * 100) / max_c;
        self.emit_status(
            "lifecycle",
            &format!(
                "Preflight compression complete: {}% -> {}% context usage",
                before_pct, after_pct
            ),
        );
    }

    fn emit_status(&self, event_type: &str, message: &str) {
        if self.config.quiet_mode {
            return;
        }
        if let Some(cb) = self.callbacks.status_callback.as_ref() {
            cb(event_type, message);
        }
    }

    fn should_emit_context_pressure_warning(
        progress_ratio: f64,
        tier: f64,
        warned_tier: &mut f64,
        last_warn_at: &mut Option<Instant>,
        last_warn_percent: &mut f64,
    ) -> bool {
        if tier <= 0.0 {
            return false;
        }
        let progress_percent = progress_ratio * 100.0;
        let now = Instant::now();
        const WARN_COOLDOWN_SECS: u64 = 20;
        const WARN_PERCENT_STEP: f64 = 5.0;

        let tier_upgraded = tier > *warned_tier;
        let cooldown_elapsed = last_warn_at
            .map(|t| now.duration_since(t) >= Duration::from_secs(WARN_COOLDOWN_SECS))
            .unwrap_or(true);
        let percent_advanced = (progress_percent - *last_warn_percent) >= WARN_PERCENT_STEP;

        if tier_upgraded || (cooldown_elapsed && percent_advanced) {
            if tier_upgraded {
                *warned_tier = tier;
            }
            *last_warn_at = Some(now);
            *last_warn_percent = progress_percent;
            return true;
        }
        false
    }

    fn assistant_visible_text(m: &Message) -> bool {
        m.content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
        let Some(content) = m.content.as_deref() else {
            return false;
        };
        !strip_think_blocks_for_ack(content).trim().is_empty()
    }

    fn coerce_textual_tool_calls(mut m: Message) -> (Message, Vec<ToolCall>, bool) {
        let declared = m.tool_calls.clone().unwrap_or_default();
        if !declared.is_empty() {
            return (m, declared, false);
        }
        let Some(content) = m.content.as_deref() else {
            return (m, Vec::new(), false);
        };
        let (plain_text, parsed_calls) = separate_text_and_calls(content);
        if parsed_calls.is_empty() {
            return (m, Vec::new(), false);
        }
        m.tool_calls = Some(parsed_calls.clone());
        let trimmed = plain_text.trim();
        m.content = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        (m, parsed_calls, true)
    }

    fn assistant_has_reasoning(m: &Message) -> bool {
        m.reasoning_content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    fn normalize_tool_call_arguments(tc: &mut ToolCall) -> Result<(), String> {
        let (normalized, repair) = repair_tool_call_arguments(&tc.function.arguments);
        if repair != ToolArgumentRepair::Unchanged {
            tracing::warn!(
                "repaired tool-call arguments for tool={} repair={:?}",
                tc.function.name,
                repair
            );
        }
        serde_json::from_str::<Value>(&normalized)
            .map(|_| {
                tc.function.arguments = normalized;
            })
            .map_err(|e| e.to_string())
    }

    fn extra_body_for_api_mode(&self, api_mode: &ApiMode) -> Option<Value> {
        let mut body = self
            .config
            .extra_body
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
        if !body.is_object() {
            return self.config.extra_body.clone();
        }
        if !matches!(api_mode, ApiMode::CodexResponses) {
            if body.get("strict_tool_calls").is_none()
                && body.get("strict_api").is_none()
                && body.get("provider_strict").is_none()
            {
                body["strict_api"] = Value::Bool(true);
            }
        }
        Some(body)
    }

    // -- Retry-aware LLM call ---------------------------------------------

    fn call_llm_with_retry<'a>(
        &'a self,
        ctx: &'a ContextManager,
        tool_schemas: &'a [ToolSchema],
        route: Option<&'a TurnRuntimeRoute>,
        max_tokens_override: Option<u32>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<hermes_core::LlmResponse, AgentError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.call_llm_with_retry_inner(ctx, tool_schemas, route, max_tokens_override))
    }

    async fn call_llm_with_retry_inner(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        max_tokens_override: Option<u32>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        let model = route
            .map(|r| r.model.as_str())
            .unwrap_or(self.config.model.as_str());
        let (inferred_provider, model_name) = self.extract_provider_and_model(model);
        let route_provider_hint = route
            .and_then(|r| r.provider.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let active_provider = route_provider_hint.unwrap_or(inferred_provider);
        let active_base_url = route
            .and_then(|r| r.base_url.clone())
            .or_else(|| self.resolve_runtime_base_url(active_provider.as_str(), None));
        // Always try the requested model first. Some providers only reveal tool
        // schema limitations at request time, so proactive substitution hides
        // the real model behavior and makes quorum voters appear to "succeed"
        // on a different backend.
        let effective_model_name = model_name.to_string();
        if let Some(rt) = route {
            if let Some(ref label) = rt.route_label {
                tracing::debug!(%label, model = %rt.model, ?rt.signature, "smart model route");
            }
            if rt.command.is_some() || !rt.args.is_empty() {
                tracing::debug!(command = ?rt.command, args = ?rt.args, "smart route process metadata");
            }
        }
        let api_messages = self.messages_for_api_call(ctx);
        let retry = &self.config.retry;
        let (effective_max_retries, effective_base_delay_ms) =
            (retry.max_retries, retry.base_delay_ms);
        let default_extra_body = self.extra_body_for_api_mode(&self.config.api_mode);
        let effective_max_tokens = max_tokens_override.or(self.config.max_tokens);

        for attempt in 0..=effective_max_retries {
            self.interrupt.check_interrupt()?;
            let result = if let Some(rt) = route {
                let (provider_name, _) = self.extract_provider_and_model(model);
                let runtime_provider_name = rt.provider.as_deref().unwrap_or(provider_name.as_str());
                let wire_model_name =
                    Self::runtime_wire_model_for_provider(runtime_provider_name, &effective_model_name);
                let mode = rt.api_mode.as_ref().unwrap_or(&self.config.api_mode);
                let extra_body_for_call = self.extra_body_for_api_mode(mode);
                let pool = self.credential_pool_for_route(rt);
                let routed_provider = self.build_runtime_provider(
                    runtime_provider_name,
                    &effective_model_name,
                    rt.base_url.as_deref(),
                    rt.api_key_env.as_deref(),
                    None,
                    Some(mode),
                    pool,
                );
                match routed_provider {
                    Ok(provider) => {
                        provider
                            .chat_completion(
                                &api_messages,
                                tool_schemas,
                                effective_max_tokens,
                                self.config.temperature,
                                Some(&wire_model_name),
                                extra_body_for_call.as_ref(),
                            )
                            .await
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Runtime route unavailable (reason={:?}), falling back to primary runtime: {}",
                            rt.routing_reason,
                            e
                        );
                        let fallback_wire_model_name = Self::runtime_wire_model_for_provider(
                            active_provider.as_str(),
                            &effective_model_name,
                        );
                        self.llm_provider
                            .chat_completion(
                                &api_messages,
                                tool_schemas,
                                effective_max_tokens,
                                self.config.temperature,
                                Some(&fallback_wire_model_name),
                                default_extra_body.as_ref(),
                            )
                            .await
                    }
                }
            } else {
                let wire_model_name = Self::runtime_wire_model_for_provider(
                    active_provider.as_str(),
                    &effective_model_name,
                );
                self.llm_provider
                    .chat_completion(
                        &api_messages,
                        tool_schemas,
                        effective_max_tokens,
                        self.config.temperature,
                        Some(&wire_model_name),
                        default_extra_body.as_ref(),
                    )
                    .await
            };

            match result {
                Ok(response) => return Ok(response),
                Err(e) => {
                    let err_str = e.to_string();
                    let class = classify_error(&err_str);
                    tracing::warn!(
                        attempt,
                        error_class = ?class,
                        "LLM API error: {}",
                        &err_str[..err_str.len().min(200)]
                    );

                    match class {
                        ErrorClass::Auth => {
                            if let Some(diag) = maybe_nous_401_diagnostic(
                                active_provider.as_str(),
                                &err_str,
                                self.config.hermes_home.as_deref(),
                            ) {
                                self.emit_status("lifecycle", &diag);
                                return Err(AgentError::LlmApi(format!("{err_str}\n\n{diag}")));
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::Fatal => {
                            if !tool_schemas.is_empty()
                                && is_tool_payload_validation_error(&err_str)
                            {
                                let (provider_name, model_name) =
                                    self.extract_provider_and_model(model);
                                if let Some(fallback_model_name) =
                                    preferred_tool_payload_fallback_model(
                                        active_provider.as_str(),
                                        model_name,
                                    )
                                {
                                    if !fallback_model_name.eq_ignore_ascii_case(model_name) {
                                        tracing::warn!(
                                            "LLM rejected tool payload on {}:{}; retrying with fallback tool-capable model {}",
                                            provider_name,
                                            model_name,
                                            fallback_model_name
                                        );
                                        let fallback_with_tools = if let Some(rt) = route {
                                            let mode = rt
                                                .api_mode
                                                .as_ref()
                                                .unwrap_or(&self.config.api_mode);
                                            let extra_body_for_call =
                                                self.extra_body_for_api_mode(mode);
                                            let pool = self.credential_pool_for_route(rt);
                                            match self.build_runtime_provider(
                                                rt.provider
                                                    .as_deref()
                                                    .unwrap_or(provider_name.as_str()),
                                                &fallback_model_name,
                                                rt.base_url.as_deref(),
                                                rt.api_key_env.as_deref(),
                                                None,
                                                Some(mode),
                                                pool,
                                            ) {
                                                Ok(provider) => {
                                                    provider
                                                        .chat_completion(
                                                            &api_messages,
                                                            tool_schemas,
                                                            effective_max_tokens,
                                                            self.config.temperature,
                                                            Some(&fallback_model_name),
                                                            extra_body_for_call.as_ref(),
                                                        )
                                                        .await
                                                }
                                                Err(build_err) => Err(build_err),
                                            }
                                        } else {
                                            match self.build_runtime_provider(
                                                provider_name.as_str(),
                                                &fallback_model_name,
                                                None,
                                                None,
                                                None,
                                                None,
                                                self.primary_credential_pool.as_ref(),
                                            ) {
                                                Ok(provider) => {
                                                    provider
                                                        .chat_completion(
                                                            &api_messages,
                                                            tool_schemas,
                                                            effective_max_tokens,
                                                            self.config.temperature,
                                                            Some(&fallback_model_name),
                                                            default_extra_body.as_ref(),
                                                        )
                                                        .await
                                                }
                                                Err(build_err) => Err(build_err),
                                            }
                                        };
                                        match fallback_with_tools {
                                            Ok(resp) => {
                                                self.emit_status(
                                                    "lifecycle",
                                                    &format!(
                                                        "Model/tool-schema mismatch on {}:{}; auto-routed to {} for this turn",
                                                        provider_name, model_name, fallback_model_name
                                                    ),
                                                );
                                                return Ok(resp);
                                            }
                                            Err(fallback_err) => {
                                                tracing::warn!(
                                                    "Fallback tool-capable retry failed: {}",
                                                    fallback_err
                                                );
                                            }
                                        }
                                    }
                                }

                                tracing::warn!(
                                    "LLM rejected tool payload; retrying once without tools"
                                );
                                let no_tools_result = if let Some(rt) = route {
                                    let mode =
                                        rt.api_mode.as_ref().unwrap_or(&self.config.api_mode);
                                    let extra_body_for_call = self.extra_body_for_api_mode(mode);
                                    let pool = self.credential_pool_for_route(rt);
                                    let runtime_provider_name =
                                        rt.provider.as_deref().unwrap_or(provider_name.as_str());
                                    let wire_model_name = Self::runtime_wire_model_for_provider(
                                        runtime_provider_name,
                                        model_name,
                                    );
                                    match self.build_runtime_provider(
                                        runtime_provider_name,
                                        model_name,
                                        rt.base_url.as_deref(),
                                        rt.api_key_env.as_deref(),
                                        None,
                                        Some(mode),
                                        pool,
                                    ) {
                                        Ok(provider) => {
                                            provider
                                                .chat_completion(
                                                    &api_messages,
                                                    &[],
                                                    effective_max_tokens,
                                                    self.config.temperature,
                                                    Some(&wire_model_name),
                                                    extra_body_for_call.as_ref(),
                                                )
                                                .await
                                        }
                                        Err(_) => {
                                            let (fallback_provider_name, fallback_model_name) = self
                                                .extract_provider_and_model(
                                                    self.config.model.as_str(),
                                                );
                                            let fallback_wire_model_name =
                                                Self::runtime_wire_model_for_provider(
                                                    fallback_provider_name.as_str(),
                                                    fallback_model_name,
                                                );
                                            self.llm_provider
                                                .chat_completion(
                                                    &api_messages,
                                                    &[],
                                                    effective_max_tokens,
                                                    self.config.temperature,
                                                    Some(&fallback_wire_model_name),
                                                    default_extra_body.as_ref(),
                                                )
                                                .await
                                        }
                                    }
                                } else {
                                    let wire_model_name = Self::runtime_wire_model_for_provider(
                                        provider_name.as_str(),
                                        model_name,
                                    );
                                    self.llm_provider
                                        .chat_completion(
                                            &api_messages,
                                            &[],
                                            effective_max_tokens,
                                            self.config.temperature,
                                            Some(&wire_model_name),
                                            default_extra_body.as_ref(),
                                        )
                                        .await
                                };
                                match no_tools_result {
                                    Ok(resp) => {
                                        self.emit_status(
                                            "lifecycle",
                                            "Model/tool-schema mismatch detected; retried once without tools for this turn",
                                        );
                                        return Ok(resp);
                                    }
                                    Err(no_tools_err) => {
                                        return Err(AgentError::LlmApi(no_tools_err.to_string()));
                                    }
                                }
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::ContextOverflow => {
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::RateLimit | ErrorClass::Retryable => {
                            let pool_may_recover = if class == ErrorClass::RateLimit {
                                let active_pool = match route {
                                    Some(rt) => self.credential_pool_for_route(rt),
                                    None => self.primary_credential_pool.as_ref(),
                                };
                                credential_pool_may_recover_from_rate_limit(
                                    active_pool,
                                    active_provider.as_str(),
                                    active_base_url.as_deref(),
                                )
                            } else {
                                false
                            };
                            let configured_failover = !retry.fallback_models.is_empty()
                                || retry
                                    .fallback_model
                                    .as_deref()
                                    .map(str::trim)
                                    .map(|m| !m.is_empty())
                                    .unwrap_or(false);
                            let eager_failover = class == ErrorClass::RateLimit
                                && !pool_may_recover
                                && configured_failover;
                            let failover_chain =
                                if attempt >= effective_max_retries || eager_failover {
                                    self.resolve_retry_failover_chain(model)
                                } else {
                                    Vec::new()
                                };
                            if attempt >= effective_max_retries
                                || (eager_failover && !failover_chain.is_empty())
                            {
                                if !failover_chain.is_empty() {
                                    let mut failover_errors = Vec::new();
                                    for fallback in failover_chain {
                                        let reason = if eager_failover {
                                            "Rate limited; credential pool rotation cannot recover"
                                        } else {
                                            "All retries exhausted"
                                        };
                                        tracing::info!(
                                            "{} on {}. Trying fallback: {}",
                                            reason,
                                            model,
                                            fallback
                                        );
                                        let (fallback_provider_name, fallback_model_name) =
                                            self.extract_provider_and_model(&fallback);
                                        let fallback_wire_model_name =
                                            Self::runtime_wire_model_for_provider(
                                                fallback_provider_name.as_str(),
                                                fallback_model_name,
                                            );
                                        let fallback_result = self
                                            .llm_provider
                                            .chat_completion(
                                                &api_messages,
                                                tool_schemas,
                                                effective_max_tokens,
                                                self.config.temperature,
                                                Some(&fallback_wire_model_name),
                                                default_extra_body.as_ref(),
                                            )
                                            .await;
                                        match fallback_result {
                                            Ok(resp) => {
                                                self.emit_status(
                                                    "lifecycle",
                                                    &format!(
                                                        "{}; failover recovered request via {}",
                                                        reason, fallback
                                                    ),
                                                );
                                                return Ok(resp);
                                            }
                                            Err(err) => {
                                                failover_errors
                                                    .push(format!("{} => {}", fallback, err));
                                            }
                                        }
                                    }
                                    return Err(AgentError::LlmApi(format!(
                                        "{} | failover attempts failed: {}",
                                        err_str,
                                        failover_errors.join(" ; ")
                                    )));
                                }
                                return Err(AgentError::LlmApi(err_str));
                            }
                            let delay = jittered_backoff(
                                attempt,
                                effective_base_delay_ms,
                                retry.max_delay_ms,
                            );
                            tracing::info!(
                                "Retrying in {}ms (attempt {}/{})",
                                delay.as_millis(),
                                attempt + 1,
                                effective_max_retries
                            );
                            self.emit_status(
                                "lifecycle",
                                &format!(
                                    "LLM API retry in {}ms (attempt {}/{})",
                                    delay.as_millis(),
                                    attempt + 1,
                                    effective_max_retries
                                ),
                            );
                            sleep(delay).await;
                        }
                    }
                }
            }
        }
        unreachable!()
    }

    fn assemble_stream_assistant_message(
        content: &str,
        reasoning_content: &str,
        tool_calls: &[ToolCall],
    ) -> Message {
        if tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.function.name.is_empty()) {
            let mut m = Message::assistant(content.to_string());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        } else {
            let content_opt = if content.is_empty() {
                None
            } else {
                Some(content.to_string())
            };
            let mut m = Message::assistant_with_tool_calls(content_opt, tool_calls.to_vec());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        }
    }

    /// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).
    async fn collect_stream_llm_response(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        active_model: &str,
        max_tokens_override: Option<u32>,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<StreamCollectOutcome, AgentError> {
        let api_messages = self.messages_for_api_call(ctx);
        let (active_provider_name, active_model_name) = self.extract_provider_and_model(active_model);
        let default_extra_body = self.extra_body_for_api_mode(&self.config.api_mode);
        let effective_max_tokens = max_tokens_override.or(self.config.max_tokens);
        let max_stream_retries = std::env::var("HERMES_STREAM_RETRIES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .map(|v| v.min(10))
            .unwrap_or(self.config.stream_read_max_retries.min(10));

        'stream_attempt: for stream_attempt in 0..=max_stream_retries {
            let mut stream = if let Some(rt) = route {
                let (provider_name, model_name) = self.extract_provider_and_model(active_model);
                let runtime_provider_name = rt.provider.as_deref().unwrap_or(provider_name.as_str());
                let wire_model_name =
                    Self::runtime_wire_model_for_provider(runtime_provider_name, model_name);
                let mode = rt.api_mode.as_ref().unwrap_or(&self.config.api_mode);
                let extra_body_for_call = self.extra_body_for_api_mode(mode);
                let pool = self.credential_pool_for_route(rt);
                match self.build_runtime_provider(
                    runtime_provider_name,
                    model_name,
                    rt.base_url.as_deref(),
                    rt.api_key_env.as_deref(),
                    None,
                    Some(mode),
                    pool,
                ) {
                    Ok(provider) => provider.chat_completion_stream(
                        &api_messages,
                        tool_schemas,
                        effective_max_tokens,
                        self.config.temperature,
                        Some(&wire_model_name),
                        extra_body_for_call.as_ref(),
                    ),
                    Err(e) => {
                        tracing::warn!(
                            "Runtime route unavailable (reason={:?}) for stream, falling back to primary runtime: {}",
                            rt.routing_reason,
                            e
                        );
                        let (fallback_provider_name, fallback_model_name) =
                            self.extract_provider_and_model(self.config.model.as_str());
                        let fallback_wire_model_name = Self::runtime_wire_model_for_provider(
                            fallback_provider_name.as_str(),
                            fallback_model_name,
                        );
                        self.llm_provider.chat_completion_stream(
                            &api_messages,
                            tool_schemas,
                            effective_max_tokens,
                            self.config.temperature,
                            Some(&fallback_wire_model_name),
                            default_extra_body.as_ref(),
                        )
                    }
                }
            } else {
                let wire_model_name = Self::runtime_wire_model_for_provider(
                    active_provider_name.as_str(),
                    active_model_name,
                );
                self.llm_provider.chat_completion_stream(
                    &api_messages,
                    tool_schemas,
                    effective_max_tokens,
                    self.config.temperature,
                    Some(&wire_model_name),
                    default_extra_body.as_ref(),
                )
            };

            let mut content = String::new();
            let mut reasoning_content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut last_usage: Option<UsageStats> = None;
            let mut finish_reason: Option<String> = None;
            let mut deltas_were_sent = false;
            let mut stream_scrubber = StreamingContextScrubber::new();

            while let Some(chunk_result) = stream.next().await {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    let message = Self::assemble_stream_assistant_message(
                        &content,
                        &reasoning_content,
                        &tool_calls,
                    );
                    return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                        message,
                        usage: last_usage.clone(),
                        model: active_model.to_string(),
                        finish_reason: Some("interrupted".to_string()),
                    }));
                }

                let chunk = match chunk_result {
                    Ok(chunk) => chunk,
                    Err(err) => {
                        let partial_tool_in_flight = tool_calls.iter().any(|tc| {
                            !tc.id.is_empty()
                                || !tc.function.name.is_empty()
                                || !tc.function.arguments.trim().is_empty()
                        });
                        let should_retry_for_partial_tool = deltas_were_sent
                            && partial_tool_in_flight
                            && is_transient_stream_error(&err)
                            && stream_attempt < max_stream_retries;
                        let should_retry_before_deltas = !deltas_were_sent
                            && is_transient_stream_error(&err)
                            && stream_attempt < max_stream_retries;

                        if should_retry_for_partial_tool || should_retry_before_deltas {
                            let next_attempt = stream_attempt + 2;
                            let total_attempts = max_stream_retries + 1;
                            if should_retry_for_partial_tool {
                                on_chunk(StreamChunk {
                                    delta: Some(hermes_core::StreamDelta {
                                        content: Some(
                                            "\n\n[connection dropped mid tool-call; reconnecting...]\n\n"
                                                .to_string(),
                                        ),
                                        tool_calls: None,
                                        extra: None,
                                    }),
                                    finish_reason: None,
                                    usage: None,
                                });
                                self.emit_status(
                                    "lifecycle",
                                    &format!(
                                        "Connection dropped mid tool-call; reconnecting (attempt {}/{})",
                                        next_attempt, total_attempts
                                    ),
                                );
                                tracing::warn!(
                                    "Streaming attempt {}/{} failed after partial tool-call data; retrying: {}",
                                    stream_attempt + 1,
                                    total_attempts,
                                    err
                                );
                            } else {
                                tracing::warn!(
                                    "Streaming attempt {}/{} failed before deltas; retrying: {}",
                                    stream_attempt + 1,
                                    total_attempts,
                                    err
                                );
                            }
                            continue 'stream_attempt;
                        }
                        return Err(err);
                    }
                };

                let mut emit_chunk = chunk.clone();
                if let Some(ref delta) = chunk.delta {
                    if let Some(ref text) = delta.content {
                        let scrubbed = stream_scrubber.feed(text);
                        if let Some(ref mut emit_delta) = emit_chunk.delta {
                            emit_delta.content = if scrubbed.is_empty() {
                                None
                            } else {
                                Some(scrubbed.clone())
                            };
                        }
                        if !scrubbed.is_empty() {
                            deltas_were_sent = true;
                            content.push_str(&scrubbed);
                            if let Some(ref cb) = self.callbacks.on_stream_delta {
                                cb(&scrubbed);
                            }
                        }
                    }
                    if let Some(ref extra) = delta.extra {
                        if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                            reasoning_content.push_str(thinking);
                            if let Some(ref cb) = self.callbacks.on_thinking {
                                cb(thinking);
                            }
                        }
                    }
                    if let Some(ref tc_deltas) = delta.tool_calls {
                        for tcd in tc_deltas {
                            let idx = tcd.index as usize;
                            while tool_calls.len() <= idx {
                                tool_calls.push(ToolCall {
                                    id: String::new(),
                                    function: hermes_core::FunctionCall {
                                        name: String::new(),
                                        arguments: String::new(),
                                    },
                                    extra_content: None,
                                });
                            }
                            if let Some(ref id) = tcd.id {
                                tool_calls[idx].id = id.clone();
                            }
                            if let Some(ref fc) = tcd.function {
                                if let Some(ref name) = fc.name {
                                    tool_calls[idx].function.name = name.clone();
                                }
                                if let Some(ref args) = fc.arguments {
                                    tool_calls[idx].function.arguments.push_str(args);
                                }
                            }
                        }
                    }
                }

                if let Some(ref usage) = chunk.usage {
                    last_usage = Some(usage.clone());
                }
                if let Some(ref fr) = chunk.finish_reason {
                    finish_reason = Some(fr.clone());
                }

                if chunk.finish_reason.is_some() {
                    let scrubbed_tail = stream_scrubber.flush();
                    if !scrubbed_tail.is_empty() {
                        content.push_str(&scrubbed_tail);
                        if let Some(ref cb) = self.callbacks.on_stream_delta {
                            cb(&scrubbed_tail);
                        }
                        on_chunk(StreamChunk {
                            delta: Some(hermes_core::StreamDelta {
                                content: Some(scrubbed_tail),
                                tool_calls: None,
                                extra: None,
                            }),
                            finish_reason: None,
                            usage: None,
                        });
                    }
                }

                let empty_delta = emit_chunk.delta.as_ref().is_some_and(|delta| {
                    delta.content.is_none() && delta.tool_calls.is_none() && delta.extra.is_none()
                });
                if !empty_delta || emit_chunk.finish_reason.is_some() || emit_chunk.usage.is_some()
                {
                    on_chunk(emit_chunk);
                }
            }

            let scrubbed_tail = stream_scrubber.flush();
            if !scrubbed_tail.is_empty() {
                content.push_str(&scrubbed_tail);
                if let Some(ref cb) = self.callbacks.on_stream_delta {
                    cb(&scrubbed_tail);
                }
                on_chunk(StreamChunk {
                    delta: Some(hermes_core::StreamDelta {
                        content: Some(scrubbed_tail),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }

            for tc in &mut tool_calls {
                let (normalized, _) = repair_tool_call_arguments(&tc.function.arguments);
                tc.function.arguments = normalized;
            }

            let has_truncated_tool_args = tool_calls.iter().any(|tc| {
                let trimmed = tc.function.arguments.trim();
                !trimmed.is_empty() && serde_json::from_str::<Value>(trimmed).is_err()
            });
            if has_truncated_tool_args {
                finish_reason = Some("length".to_string());
            }

            let message =
                Self::assemble_stream_assistant_message(&content, &reasoning_content, &tool_calls);

            return Ok(StreamCollectOutcome::Complete(LlmResponse {
                message,
                usage: last_usage,
                model: active_model.to_string(),
                finish_reason,
            }));
        }

        Err(AgentError::LlmApi(
            "streaming failed after retry budget exhausted".to_string(),
        ))
    }

}
