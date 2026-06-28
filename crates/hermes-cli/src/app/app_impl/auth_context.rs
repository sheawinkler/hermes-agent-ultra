impl App {
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

}
