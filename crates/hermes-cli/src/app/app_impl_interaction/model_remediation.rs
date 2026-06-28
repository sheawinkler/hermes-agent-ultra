impl App {
    fn model_auto_remediation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_MODEL_AUTO_REMEDIATE")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn is_model_not_found_error(err: &AgentError) -> bool {
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

    fn is_provider_auth_or_session_error(err: &AgentError) -> bool {
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

    fn is_provider_tool_payload_error(err: &AgentError) -> bool {
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

    fn quorum_output_is_degraded_non_answer(output: &str) -> bool {
        let lower = output.to_ascii_lowercase();
        lower.contains("objective delivery compromised")
            || lower.contains("reverting to hermes")
            || lower.contains("safe-mode response")
            || lower.contains("safe mode response")
            || (lower.contains("i do not have") && lower.contains("tools"))
            || (lower.contains("cannot access") && lower.contains("tools"))
    }

    async fn force_auth_refresh_after_error(&mut self) -> bool {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        let provider = normalize_runtime_provider_name(provider_name.as_str());
        let (notice, refreshed) = match provider.as_str() {
            "nous" => match resolve_nous_runtime_credentials(
                true,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    let changed = Self::apply_nous_runtime_credentials(&creds);
                    if changed {
                        self.switch_model(&self.current_model.clone());
                    }
                    (
                        Some("Nous auth auto-refresh succeeded; retrying request.".to_string()),
                        true,
                    )
                }
                Err(err) => {
                    if Self::nous_refresh_contention_error(&err) {
                        match resolve_nous_runtime_credentials(
                            false,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                let changed = Self::apply_nous_runtime_credentials(&creds);
                                if changed {
                                    self.switch_model(&self.current_model.clone());
                                }
                                (
                                    Some(
                                        "Nous refresh busy; reused cached runtime credential and retrying request."
                                            .to_string(),
                                    ),
                                    true,
                                )
                            }
                            Err(cache_err) => (
                                Some(format!(
                                    "Nous cached credential hydration failed after refresh contention: {}",
                                    cache_err
                                )),
                                false,
                            ),
                        }
                    } else if Self::auth_error_requires_nous_login(&err)
                        && self
                            .attempt_interactive_nous_login("runtime auth refresh failed")
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
                                let changed = Self::apply_nous_runtime_credentials(&creds);
                                if changed {
                                    self.switch_model(&self.current_model.clone());
                                }
                                (
                                    Some(
                                        "Nous auth re-login succeeded; retrying request."
                                            .to_string(),
                                    ),
                                    true,
                                )
                            }
                            Err(retry_err) => (
                                Some(format!("Nous auth auto-refresh failed: {}", retry_err)),
                                false,
                            ),
                        }
                    } else {
                        (
                            Some(format!("Nous auth auto-refresh failed: {}", err)),
                            false,
                        )
                    }
                }
            },
            "qwen-oauth" => {
                match resolve_qwen_runtime_credentials(
                    true,
                    true,
                    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                )
                .await
                {
                    Ok(creds) => {
                        let mut changed = false;
                        changed |=
                            Self::set_env_if_changed("HERMES_QWEN_OAUTH_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("DASHSCOPE_API_KEY", &creds.api_key);
                        if !creds.base_url.trim().is_empty() {
                            changed |=
                                Self::set_env_if_changed("HERMES_QWEN_BASE_URL", &creds.base_url);
                        }
                        if changed {
                            self.switch_model(&self.current_model.clone());
                        }
                        (
                            Some(
                                "Qwen OAuth auto-refresh succeeded; retrying request.".to_string(),
                            ),
                            true,
                        )
                    }
                    Err(err) => (
                        Some(format!("Qwen OAuth auto-refresh failed: {}", err)),
                        false,
                    ),
                }
            }
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                match resolve_gemini_oauth_runtime_credentials(true).await {
                    Ok(creds) => {
                        let mut changed = false;
                        changed |=
                            Self::set_env_if_changed("HERMES_GEMINI_OAUTH_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("GOOGLE_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("GEMINI_API_KEY", &creds.api_key);
                        if changed {
                            self.switch_model(&self.current_model.clone());
                        }
                        (
                            Some(
                                "Gemini OAuth auto-refresh succeeded; retrying request."
                                    .to_string(),
                            ),
                            true,
                        )
                    }
                    Err(err) => (
                        Some(format!("Gemini OAuth auto-refresh failed: {}", err)),
                        false,
                    ),
                }
            }
            _ => (None, false),
        };

        if let Some(text) = notice {
            Self::emit_lifecycle_event(&self.stream_handle_shared, &text);
            if self.stream_handle.is_some() {
                self.push_ui_assistant(text);
            } else {
                println!("{}", text);
            }
        }
        refreshed
    }

    async fn model_auto_remediation_target(&self, err: &AgentError) -> Option<(String, String)> {
        if !Self::model_auto_remediation_enabled() || !Self::is_model_not_found_error(err) {
            return None;
        }

        let (provider, current_model_id) = self
            .current_model
            .split_once(':')
            .unwrap_or(("openai", self.current_model.as_str()));
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
        if next_model.eq_ignore_ascii_case(&self.current_model) {
            return None;
        }
        let close = Self::rank_catalog_candidates(current_model_id, &catalog, 3);
        let notice = format!(
            "Model catalog remediation: `{}` failed with not-found; switching to `{}` and retrying once. close matches: {}",
            self.current_model,
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
