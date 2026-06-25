//! Retry-aware LLM calls (parity with agent/chat_completion_helpers.py).

use std::time::Instant;

use hermes_core::{AgentError, ToolSchema};
use tokio::time::sleep;

use crate::agent_loop::{
    AgentLoop, ErrorClass, TurnRuntimeRoute, classify_error, is_tool_payload_validation_error,
    jittered_backoff, maybe_nous_401_diagnostic, preferred_tool_payload_fallback_model,
};
use crate::context::ContextManager;

impl AgentLoop {
    // -- Retry-aware LLM call ---------------------------------------------

    pub(crate) fn call_llm_with_retry<'a>(
        &'a self,
        ctx: &'a mut ContextManager,
        tool_schemas: &'a [ToolSchema],
        route: Option<&'a TurnRuntimeRoute>,
        max_tokens_override: Option<u32>,
        api_call_count: &'a mut u32,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<hermes_core::LlmResponse, AgentError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.call_llm_with_retry_inner(
            ctx,
            tool_schemas,
            route,
            max_tokens_override,
            api_call_count,
        ))
    }

    pub(crate) async fn call_llm_with_retry_inner(
        &self,
        ctx: &mut ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        max_tokens_override: Option<u32>,
        api_call_count: &mut u32,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        let default_model = crate::runtime_provider::active_model(self);
        let model = route
            .map(|r| r.model.as_str())
            .unwrap_or(default_model.as_str());
        let (inferred_provider, model_name) =
            crate::route_learning::extract_provider_and_model(self, model);
        let route_provider_hint = route
            .and_then(|r| r.provider.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let active_provider = route_provider_hint.unwrap_or(inferred_provider);
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
        let retry = self.config().retry.clone();
        let (effective_max_retries, effective_base_delay_ms) =
            (retry.max_retries, retry.base_delay_ms);
        let active_runtime = crate::route_learning::primary_runtime_snapshot(self);
        let default_api_mode = active_runtime.api_mode.clone();
        let default_extra_body =
            crate::llm_caller::extra_body_for_api_mode(self, &default_api_mode);
        let effective_max_tokens = max_tokens_override.or(self.config().max_tokens);
        let mut context_overflow_retries = 0u32;
        let mut has_retried_429_same_cred = false;
        let mut auth_refresh_attempted = false;
        let mut thinking_sig_retry_attempted = false;

        if active_provider == "nous" {
            if let Some(remaining) = crate::nous_rate_guard::nous_rate_limit_remaining(
                self.config().hermes_home.as_deref(),
            ) {
                if remaining > 0.0 {
                    let msg = format!(
                        "Nous Portal rate limit active — resets in {}.",
                        crate::nous_rate_guard::format_remaining(remaining)
                    );
                    tracing::info!(%msg, "nous rate guard: skipping API call");
                    hermes_telemetry::record_nous_rate_limit_skip();
                    if self.try_activate_session_fallback(&effective_model_name) {
                        return self
                            .call_llm_with_retry(
                                ctx,
                                tool_schemas,
                                route,
                                max_tokens_override,
                                api_call_count,
                            )
                            .await;
                    }
                    return Err(hermes_core::AgentError::RateLimited {
                        retry_after_secs: Some(remaining.ceil() as u64),
                    });
                }
            }
        }

        for attempt in 0..=effective_max_retries {
            let api_messages = crate::llm_caller::build_turn_api_messages(self, ctx);
            self.interrupt.check_interrupt()?;
            let api_start = Instant::now();
            *api_call_count = api_call_count.saturating_add(1);
            let hook_api_mode = route
                .and_then(|rt| rt.api_mode.as_ref())
                .unwrap_or(&default_api_mode);
            let hook_base_url = crate::runtime_provider::resolve_runtime_base_url(
                self,
                active_provider.as_str(),
                route.and_then(|rt| rt.base_url.as_deref()),
            );
            crate::hooks::invoke_pre_api_request_hook(
                self,
                *api_call_count,
                &api_messages,
                tool_schemas.len(),
                model,
                active_provider.as_str(),
                hook_base_url.as_deref(),
                hook_api_mode,
                effective_max_tokens,
            );
            let result = if let Some(rt) = route {
                let (provider_name, _) =
                    crate::route_learning::extract_provider_and_model(self, model);
                let mode = rt.api_mode.as_ref().unwrap_or(&default_api_mode);
                let extra_body_for_call = crate::llm_caller::extra_body_for_api_mode(self, mode);
                let pool = crate::runtime_provider::credentials_pool_for_route(self, rt);
                let routed_provider = crate::runtime_provider::build_runtime_provider(
                    self,
                    rt.provider.as_deref().unwrap_or(provider_name.as_str()),
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
                                self.config().temperature,
                                Some(&effective_model_name),
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
                        crate::runtime_provider::effective_llm_provider(self)
                            .chat_completion(
                                &api_messages,
                                tool_schemas,
                                effective_max_tokens,
                                self.config().temperature,
                                Some(&effective_model_name),
                                default_extra_body.as_ref(),
                            )
                            .await
                    }
                }
            } else {
                crate::runtime_provider::effective_llm_provider(self)
                    .chat_completion(
                        &api_messages,
                        tool_schemas,
                        effective_max_tokens,
                        self.config().temperature,
                        Some(&effective_model_name),
                        default_extra_body.as_ref(),
                    )
                    .await
            };

            match result {
                Ok(mut response) => {
                    hermes_telemetry::record_llm_request();
                    hermes_telemetry::record_llm_latency(api_start.elapsed());
                    if active_provider == "nous" {
                        if let Some(headers) = response.rate_limit_headers.take() {
                            if let Ok(mut state) = self.state.lock() {
                                state.last_nous_rate_limit_headers = Some(headers);
                            }
                        }
                    }
                    // Cache diagnostics: capture prefix shape, compare with
                    // previous turn, log hit/miss breakdown.
                    if let Ok(mut state) = self.state.lock() {
                        let prev = state.last_prefix_shape.clone();
                        let s_hit = state.session_cache_hit;
                        let s_miss = state.session_cache_miss;
                        let rewrite_ver = state.compaction_count;
                        let (new_shape, _diag) = crate::cache_diagnostics::trace_turn(
                            ctx.get_messages(),
                            tool_schemas,
                            rewrite_ver,
                            response.usage.as_ref(),
                            prev.as_ref(),
                            s_hit,
                            s_miss,
                        );
                        state.last_prefix_shape = Some(new_shape);
                        state.session_cache_hit = _diag.session_hit;
                        state.session_cache_miss = _diag.session_miss;
                    }
                    return Ok(response);
                }
                Err(e) => {
                    hermes_telemetry::record_error();
                    let err_str = e.to_string();
                    if crate::vision_message_prepare::is_api_image_rejection_error(&err_str)
                        && self
                            .vision_supported
                            .load(std::sync::atomic::Ordering::Acquire)
                    {
                        tracing::warn!(
                            "API rejected image content — stripping images and retrying text-only"
                        );
                        self.disable_vision_supported_and_strip_context(ctx);
                        crate::hooks::emit_status(
                            self,
                            "lifecycle",
                            "Model rejected images — retrying without image content",
                        );
                        continue;
                    }
                    let failover = crate::error_classifier::classify_failover_reason_with_provider(
                        &err_str,
                        active_provider.as_str(),
                    );
                    if failover == crate::error_classifier::FailoverReason::ThinkingSignature
                        && !thinking_sig_retry_attempted
                    {
                        thinking_sig_retry_attempted = true;
                        crate::error_classifier::strip_thinking_blocks_from_context(ctx);
                        self.invalidate_turn_api_messages_cache();
                        tracing::warn!(
                            "Thinking block signature invalid — stripped reasoning blocks, retrying"
                        );
                        crate::hooks::emit_status(
                            self,
                            "lifecycle",
                            "Thinking block signature invalid — stripped reasoning blocks and retrying",
                        );
                        continue;
                    }
                    if failover == crate::error_classifier::FailoverReason::InvalidEncryptedReplay {
                        if crate::error_classifier::strip_invalid_encrypted_replay_from_context(ctx)
                        {
                            self.invalidate_turn_api_messages_cache();
                            tracing::warn!(
                                "Invalid encrypted reasoning replay — stripped and retrying"
                            );
                            crate::hooks::emit_status(
                                self,
                                "lifecycle",
                                "Encrypted reasoning replay invalid — stripped and retrying",
                            );
                            continue;
                        }
                    }
                    if failover == crate::error_classifier::FailoverReason::ImageTooLarge {
                        if crate::error_classifier::shrink_oversized_images_in_context(ctx) {
                            self.invalidate_turn_api_messages_cache();
                            tracing::warn!("Image too large — stripped image parts and retrying");
                            crate::hooks::emit_status(
                                self,
                                "lifecycle",
                                "Image too large — reduced image payload and retrying",
                            );
                            continue;
                        }
                    }
                    if failover == crate::error_classifier::FailoverReason::LlamaCppGrammarPattern {
                        tracing::warn!(
                            "llama.cpp grammar/schema error — retrying without tool strictness"
                        );
                        crate::hooks::emit_status(
                            self,
                            "lifecycle",
                            "Tool schema rejected by local grammar engine — retrying",
                        );
                        continue;
                    }
                    if failover
                        == crate::error_classifier::FailoverReason::OAuthLongContextBetaForbidden
                    {
                        crate::hooks::emit_status(
                            self,
                            "lifecycle",
                            "Anthropic 1M context beta unavailable for this subscription — retrying",
                        );
                        continue;
                    }
                    if failover == crate::error_classifier::FailoverReason::ProviderPolicyBlocked {
                        return Err(AgentError::LlmApi(format!(
                            "{err_str}\n\nProvider policy blocked this model endpoint. \
                             Adjust privacy/guardrail settings at the provider console."
                        )));
                    }
                    let class = if failover == crate::error_classifier::FailoverReason::Billing {
                        ErrorClass::RateLimit
                    } else {
                        classify_error(&err_str)
                    };
                    tracing::warn!(
                        attempt,
                        error_class = ?class,
                        failover = ?failover,
                        "LLM API error: {}",
                        &err_str[..err_str.len().min(200)]
                    );

                    match class {
                        ErrorClass::Auth => {
                            if !auth_refresh_attempted {
                                auth_refresh_attempted = true;
                                crate::runtime_provider::refresh_oauth_store_tokens_if_needed(self)
                                    .await;
                                tracing::info!("Auth error — refreshed OAuth tokens, retrying");
                                crate::hooks::emit_status(
                                    self,
                                    "lifecycle",
                                    "Authentication error — refreshed OAuth tokens and retrying",
                                );
                                continue;
                            }
                            if let Some(diag) = maybe_nous_401_diagnostic(
                                active_provider.as_str(),
                                &err_str,
                                self.config().hermes_home.as_deref(),
                            ) {
                                crate::hooks::emit_status(self, "lifecycle", &diag);
                                return Err(AgentError::LlmApi(format!("{err_str}\n\n{diag}")));
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::Fatal => {
                            if !tool_schemas.is_empty()
                                && is_tool_payload_validation_error(&err_str)
                            {
                                let (provider_name, model_name) =
                                    crate::route_learning::extract_provider_and_model(self, model);
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
                                            let mode =
                                                rt.api_mode.as_ref().unwrap_or(&default_api_mode);
                                            let extra_body_for_call =
                                                crate::llm_caller::extra_body_for_api_mode(
                                                    self, mode,
                                                );
                                            let pool =
                                                crate::runtime_provider::credentials_pool_for_route(
                                                    self, rt,
                                                );
                                            match crate::runtime_provider::build_runtime_provider(
                                                self,
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
                                                            self.config().temperature,
                                                            Some(&fallback_model_name),
                                                            extra_body_for_call.as_ref(),
                                                        )
                                                        .await
                                                }
                                                Err(build_err) => Err(build_err),
                                            }
                                        } else {
                                            match crate::runtime_provider::build_runtime_provider(
                                                self,
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
                                                            self.config().temperature,
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
                                                crate::hooks::emit_status(
                                                    self,
                                                    "lifecycle",
                                                    &format!(
                                                        "Model/tool-schema mismatch on {}:{}; auto-routed to {} for this turn",
                                                        provider_name,
                                                        model_name,
                                                        fallback_model_name
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
                                    let mode = rt.api_mode.as_ref().unwrap_or(&default_api_mode);
                                    let extra_body_for_call =
                                        crate::llm_caller::extra_body_for_api_mode(self, mode);
                                    let pool = crate::runtime_provider::credentials_pool_for_route(
                                        self, rt,
                                    );
                                    match crate::runtime_provider::build_runtime_provider(self,
                                        rt.provider.as_deref().unwrap_or(provider_name.as_str()),
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
                                                    self.config().temperature,
                                                    Some(model_name),
                                                    extra_body_for_call.as_ref(),
                                                )
                                                .await
                                        }
                                        Err(_) => {
                                            self.llm_provider
                                                .chat_completion(
                                                    &api_messages,
                                                    &[],
                                                    effective_max_tokens,
                                                    self.config().temperature,
                                                    Some(
                                                        crate::route_learning::extract_provider_and_model(
                                                            self,
                                                            crate::runtime_provider::active_model(self).as_str(),
                                                        )
                                                        .1,
                                                    ),
                                                    default_extra_body.as_ref(),
                                                )
                                                .await
                                        }
                                    }
                                } else {
                                    self.llm_provider
                                        .chat_completion(
                                            &api_messages,
                                            &[],
                                            effective_max_tokens,
                                            self.config().temperature,
                                            Some(model_name),
                                            default_extra_body.as_ref(),
                                        )
                                        .await
                                };
                                match no_tools_result {
                                    Ok(resp) => {
                                        crate::hooks::emit_status(
                                            self,
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
                            if context_overflow_retries == 0 {
                                context_overflow_retries = 1;
                                tracing::warn!(
                                    "Context overflow detected; compressing context and retrying in-turn"
                                );
                                crate::hooks::emit_status(
                                    self,
                                    "lifecycle",
                                    "Context window exceeded; compressing history and retrying",
                                );
                                self.compress_context(ctx).await;
                                continue;
                            }
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::RateLimit | ErrorClass::Retryable => {
                            if failover == crate::retry_failover::FailoverReason::Billing {
                                let pool = route
                                    .and_then(|rt| {
                                        crate::runtime_provider::credentials_pool_for_route(
                                            self, rt,
                                        )
                                    })
                                    .or(self.primary_credential_pool.as_ref());
                                let base_url = crate::runtime_provider::resolve_runtime_base_url(
                                    self,
                                    active_provider.as_str(),
                                    route.and_then(|rt| rt.base_url.as_deref()),
                                );
                                let pool_may_recover =
                                    crate::credential_pool_recovery::pool_may_recover_from_rate_limit(
                                        pool.map(|p| p.as_ref()),
                                        active_provider.as_str(),
                                        base_url.as_deref(),
                                    );
                                if !pool_may_recover {
                                    crate::hooks::emit_status(
                                        self,
                                        "lifecycle",
                                        "Billing or credits exhausted — switching to fallback provider",
                                    );
                                    if self.try_activate_session_fallback(&effective_model_name) {
                                        return self
                                            .call_llm_with_retry(
                                                ctx,
                                                tool_schemas,
                                                route,
                                                max_tokens_override,
                                                api_call_count,
                                            )
                                            .await;
                                    }
                                }
                            }
                            if matches!(class, ErrorClass::RateLimit) && active_provider == "nous" {
                                let parsed =
                                    crate::nous_rate_guard::parse_rate_limit_headers_from_llm_error(
                                        &err_str,
                                    );
                                let last =
                                    self.state.lock().ok().and_then(|state| {
                                        state.last_nous_rate_limit_headers.clone()
                                    });
                                let genuine = crate::nous_rate_guard::is_genuine_nous_rate_limit(
                                    parsed.as_ref(),
                                )
                                    || crate::nous_rate_guard::is_genuine_nous_rate_limit(
                                        last.as_ref(),
                                    );
                                if genuine {
                                    crate::nous_rate_guard::record_nous_rate_limit(
                                        self.config().hermes_home.as_deref(),
                                        parsed.as_ref(),
                                        None,
                                        300.0,
                                    );
                                    hermes_telemetry::record_nous_rate_limit_recorded();
                                    tracing::info!(
                                        "Nous genuine rate limit — tripping cross-session breaker"
                                    );
                                    if self.try_activate_session_fallback(&effective_model_name) {
                                        return self
                                            .call_llm_with_retry(
                                                ctx,
                                                tool_schemas,
                                                route,
                                                max_tokens_override,
                                                api_call_count,
                                            )
                                            .await;
                                    }
                                    return Err(hermes_core::AgentError::RateLimited {
                                        retry_after_secs: parsed
                                            .as_ref()
                                            .and_then(|h| {
                                                crate::nous_rate_guard::parse_reset_seconds(Some(h))
                                            })
                                            .map(|s| s.ceil() as u64),
                                    });
                                }
                            }
                            if matches!(class, ErrorClass::RateLimit) {
                                let pool = route
                                    .and_then(|rt| {
                                        crate::runtime_provider::credentials_pool_for_route(
                                            self, rt,
                                        )
                                    })
                                    .or(self.primary_credential_pool.as_ref());
                                let base_url = crate::runtime_provider::resolve_runtime_base_url(
                                    self,
                                    active_provider.as_str(),
                                    route.and_then(|rt| rt.base_url.as_deref()),
                                );
                                let error_ctx = serde_json::json!({
                                    "message": err_str,
                                });
                                let (recovered, new_flag) =
                                    crate::agent_runtime_helpers::recover_with_credential_pool(
                                        pool.map(|p| p.as_ref()),
                                        active_provider.as_str(),
                                        base_url.as_deref().unwrap_or(""),
                                        Some(429),
                                        has_retried_429_same_cred,
                                        Some(crate::error_classifier::FailoverReason::RateLimit),
                                        &error_ctx,
                                    );
                                has_retried_429_same_cred = new_flag;
                                if recovered {
                                    tracing::info!(
                                        "Rate limit: rotated credential pool entry, retrying"
                                    );
                                    crate::hooks::emit_status(
                                        self,
                                        "lifecycle",
                                        "Rate limited; rotated API credential and retrying",
                                    );
                                    continue;
                                }
                                if !crate::credential_pool_recovery::pool_may_recover_from_rate_limit(
                                    pool.map(|p| p.as_ref()),
                                    active_provider.as_str(),
                                    base_url.as_deref(),
                                ) {
                                    if attempt >= effective_max_retries {
                                        crate::runtime_provider::note_primary_rate_limited_if_applicable(self);
                                    }
                                }
                            }
                            if attempt >= effective_max_retries {
                                if matches!(class, ErrorClass::RateLimit) {
                                    crate::runtime_provider::note_primary_rate_limited_if_applicable(self);
                                }
                                let failover_chain =
                                    crate::route_learning::resolve_retry_failover_chain(
                                        self, model,
                                    );
                                if !failover_chain.is_empty() {
                                    let mut failover_errors = Vec::new();
                                    for fallback in failover_chain {
                                        if let Ok(mut state) = self.state.lock() {
                                            state.turn_fallback.fallback_chain_index = state
                                                .turn_fallback
                                                .fallback_chain_index
                                                .saturating_add(1);
                                        }
                                        tracing::info!(
                                            "All retries exhausted on {}. Trying fallback: {}",
                                            model,
                                            fallback
                                        );
                                        let failover_runtime =
                                            crate::runtime_provider::primary_runtime_for_failover_model(self, &fallback);
                                        let (_, failover_model_name) =
                                            crate::route_learning::extract_provider_and_model(
                                                self, &fallback,
                                            );
                                        let extra_body = crate::llm_caller::extra_body_for_api_mode(
                                            self,
                                            &failover_runtime.api_mode,
                                        );
                                        let mut failover_activated = false;
                                        let fallback_result = match crate::runtime_provider::build_llm_provider_for_runtime(self, &failover_runtime)
                                        {
                                            Ok(provider) => {
                                                provider
                                                    .chat_completion(
                                                        &api_messages,
                                                        tool_schemas,
                                                        effective_max_tokens,
                                                        self.config().temperature,
                                                        Some(failover_model_name),
                                                        extra_body.as_ref(),
                                                    )
                                                    .await
                                            }
                                            Err(build_err) => {
                                                tracing::warn!(
                                                    "Failover runtime provider unavailable ({build_err}); \
                                                     using session-injected provider"
                                                );
                                                self.activate_runtime_fallback(failover_runtime.clone());
                                                failover_activated = true;
                                                crate::runtime_provider::effective_llm_provider(self)
                                                    .chat_completion(
                                                        &api_messages,
                                                        tool_schemas,
                                                        effective_max_tokens,
                                                        self.config().temperature,
                                                        Some(failover_model_name),
                                                        extra_body.as_ref(),
                                                    )
                                                    .await
                                            }
                                        };
                                        match fallback_result {
                                            Ok(resp) => {
                                                if !failover_activated {
                                                    self.activate_runtime_fallback(
                                                        failover_runtime,
                                                    );
                                                }
                                                crate::hooks::emit_status(
                                                    self,
                                                    "lifecycle",
                                                    &format!(
                                                        "Failover recovered request via {}",
                                                        fallback
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
                            crate::hooks::emit_status(
                                self,
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
}
