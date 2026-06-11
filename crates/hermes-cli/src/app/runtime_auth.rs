use hermes_core::AgentError;

use crate::auth::{
    DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    NousDeviceCodeOptions, NousRuntimeCredentials, QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    login_nous_device_code, resolve_gemini_oauth_runtime_credentials,
    resolve_nous_runtime_credentials, resolve_qwen_runtime_credentials, save_nous_auth_state,
};

use super::App;
use super::provider::{
    normalize_runtime_provider_name, provider_api_key_from_env, resolve_provider_and_model,
};

impl App {
    pub(super) fn should_force_preflight_auth_refresh(provider: &str) -> bool {
        if let Some(explicit) = Self::bool_env("HERMES_FORCE_RUNTIME_AUTH_REFRESH") {
            return explicit;
        }
        matches!(
            provider,
            "nous" | "qwen-oauth" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth"
        )
    }

    pub(super) fn nous_refresh_contention_error(err: &AgentError) -> bool {
        let text = err.to_string().to_ascii_lowercase();
        text.contains("slow_down")
            || text.contains("too many requests")
            || text.contains("refresh already in progress")
            || text.contains("429")
    }

    pub(super) fn apply_nous_runtime_credentials(creds: &NousRuntimeCredentials) -> bool {
        let mut changed = false;
        changed |= Self::set_env_if_changed("NOUS_API_KEY", &creds.api_key);
        if !creds.base_url.trim().is_empty() {
            changed |= Self::set_env_if_changed("NOUS_INFERENCE_BASE_URL", &creds.base_url);
        }
        changed
    }

    pub(super) fn auto_nous_reauth_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_AUTO_NOUS_REAUTH")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    pub(super) fn auth_error_requires_nous_login(err: &AgentError) -> bool {
        let text = err.to_string().to_ascii_lowercase();
        text.contains("not logged into nous portal")
            || text.contains("re-run `hermes auth nous`")
            || text.contains("stored nous auth state is invalid")
            || text.contains("missing refresh token")
            || text.contains("invalid nous refresh response")
    }

    pub(super) async fn attempt_interactive_nous_login(&mut self, reason: &str) -> bool {
        if !Self::auto_nous_reauth_enabled() {
            return false;
        }
        Self::emit_lifecycle_event(
            &self.stream.stream_handle_shared,
            format!("Nous OAuth re-auth required ({reason}); launching portal login flow"),
        );
        match login_nous_device_code(NousDeviceCodeOptions::default()).await {
            Ok(state) => match save_nous_auth_state(&state) {
                Ok(path) => {
                    Self::emit_lifecycle_event(
                        &self.stream.stream_handle_shared,
                        format!("Nous OAuth state refreshed: {}", path.display()),
                    );
                    true
                }
                Err(err) => {
                    Self::emit_lifecycle_event(
                        &self.stream.stream_handle_shared,
                        format!("Nous OAuth state save failed: {}", err),
                    );
                    false
                }
            },
            Err(err) => {
                Self::emit_lifecycle_event(
                    &self.stream.stream_handle_shared,
                    format!("Nous OAuth interactive login failed: {}", err),
                );
                false
            }
        }
    }

    pub(super) async fn refresh_runtime_provider_credentials_if_needed(
        &mut self,
        force_refresh: bool,
    ) {
        let (provider_name, _) =
            resolve_provider_and_model(&self.core.config, &self.model.current_model);
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
                                    &self.stream.stream_handle_shared,
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
                                    &self.stream.stream_handle_shared,
                                    format!("warning: Nous credential refresh skipped ({err})"),
                                );
                            }
                        }
                    } else {
                        if !rotated && note.is_none() {
                            Self::emit_lifecycle_event(
                                &self.stream.stream_handle_shared,
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
                        &self.stream.stream_handle_shared,
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
                            &self.stream.stream_handle_shared,
                            format!("warning: Gemini OAuth refresh skipped ({e})"),
                        );
                    }
                }
            }
            _ => {}
        }

        if rotated {
            self.switch_model(&self.model.current_model.clone());
        }
        if let Some(msg) = note {
            Self::emit_lifecycle_event(&self.stream.stream_handle_shared, msg);
        }
    }

    /// Refresh and verify runtime credentials for the active provider.
    ///
    /// This is the command-surface lifecycle helper used by `/auth`.
    pub async fn verify_runtime_auth(&mut self, force_refresh: bool) -> Result<String, AgentError> {
        let provider = self.current_runtime_provider();
        let before_present = provider_api_key_from_env(&provider).is_some();
        self.refresh_runtime_provider_credentials_if_needed(force_refresh)
            .await;
        let after = provider_api_key_from_env(&provider);
        let after_present = after.is_some();
        let status = if let Some(key) = after {
            format!(
                "present (masked={} chars)",
                key.chars().count().max(1).saturating_sub(8).max(1)
            )
        } else {
            "missing".to_string()
        };
        let refresh_mode = if force_refresh { "forced" } else { "passive" };
        let changed = if before_present == after_present {
            "unchanged"
        } else {
            "updated"
        };
        Ok(format!(
            "Auth verify\nprovider: {}\nmode: {}\ncredential: {}\nstate: {}\nmodel: {}",
            provider, refresh_mode, status, changed, self.model.current_model
        ))
    }

    pub(super) async fn force_auth_refresh_after_error(&mut self) -> bool {
        let (provider_name, _) =
            resolve_provider_and_model(&self.core.config, &self.model.current_model);
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
                        self.switch_model(&self.model.current_model.clone());
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
                                    self.switch_model(&self.model.current_model.clone());
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
                                    self.switch_model(&self.model.current_model.clone());
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
                            self.switch_model(&self.model.current_model.clone());
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
                            self.switch_model(&self.model.current_model.clone());
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
            Self::emit_lifecycle_event(&self.stream.stream_handle_shared, &text);
            if self.stream.stream_handle.is_some() {
                self.push_ui_assistant(text);
            } else {
                println!("{}", text);
            }
        }
        refreshed
    }
}
