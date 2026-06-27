impl AgentLoop {
    fn extract_provider_and_model<'a>(&self, model: &'a str) -> (String, &'a str) {
        if let Some((p, m)) = model.split_once(':') {
            let p = p.trim();
            let m = m.trim();
            if !p.is_empty() && !m.is_empty() {
                return (p.to_string(), m);
            }
        }
        let fallback_provider = self
            .config
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("openai")
            .to_string();
        (fallback_provider, model)
    }

    pub(crate) fn runtime_wire_model_for_provider(provider: &str, model: &str) -> String {
        let provider = provider.trim().to_ascii_lowercase();
        let canonical = hermes_core::providers::canonical_provider_id(provider.as_str());
        let is_openai_codex_provider = matches!(provider.as_str(), "openai" | "codex" | "openai-codex")
            || matches!(canonical.as_str(), "openai" | "codex" | "openai-codex");
        if is_openai_codex_provider && is_openai_dynamic_model_alias(model) {
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL.to_string()
        } else {
            model.trim().to_string()
        }
    }

    fn runtime_provider_config(&self, provider: &str) -> Option<&RuntimeProviderConfig> {
        let provider = provider.trim();
        if provider.is_empty() {
            return None;
        }
        if let Some(cfg) = self.config.runtime_providers.get(provider) {
            return Some(cfg);
        }

        let lower = provider.to_ascii_lowercase();
        if let Some(cfg) = self.config.runtime_providers.get(lower.as_str()) {
            return Some(cfg);
        }

        let canonical = hermes_core::providers::canonical_provider_id(provider);
        if let Some(cfg) = self.config.runtime_providers.get(canonical.as_str()) {
            return Some(cfg);
        }

        let profile = crate::provider_profiles::canonical_provider_profile_id(provider);
        if let Some(profile) = profile {
            if let Some(cfg) = self.config.runtime_providers.get(profile) {
                return Some(cfg);
            }
        }

        if let Some((_, cfg)) = self
            .config
            .runtime_providers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(provider))
        {
            return Some(cfg);
        }

        if let Some(profile) = profile {
            if let Some((_, cfg)) = self.config.runtime_providers.iter().find(|(name, _)| {
                crate::provider_profiles::canonical_provider_profile_id(name) == Some(profile)
            }) {
                return Some(cfg);
            }
        }

        self.config
            .runtime_providers
            .iter()
            .find(|(name, _)| hermes_core::providers::canonical_provider_id(name) == canonical)
            .map(|(_, cfg)| cfg)
    }

    fn resolve_runtime_api_key(
        &self,
        provider: &str,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
    ) -> Option<String> {
        if provider == "copilot-acp" {
            return Some("copilot-acp".to_string());
        }
        if matches!(
            provider,
            "bedrock" | "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon"
        ) {
            return Some(BEDROCK_AUTH_MARKER.to_string());
        }
        if let Some(token) = self.resolve_oauth_store_api_key(provider) {
            return Some(token);
        }
        if let Some(key) = explicit_api_key.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(key.to_string());
        }
        if let Some(env_name) = api_key_env_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Ok(v) = std::env::var(env_name) {
                if !v.trim().is_empty() {
                    return Some(v);
                }
            }
        }
        if let Some(cfg) = self.runtime_provider_config(provider) {
            if let Some(ref key) = cfg.api_key {
                let trimmed = key.trim();
                if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}'))
                {
                    if let Ok(v) = std::env::var(env_ref) {
                        if !v.trim().is_empty() {
                            return Some(v);
                        }
                    }
                } else if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            if let Some(env_name) = cfg
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if let Ok(v) = std::env::var(env_name) {
                    if !v.trim().is_empty() {
                        return Some(v);
                    }
                }
            }
        }
        if matches!(provider, "openai" | "codex" | "openai-codex") {
            return std::env::var("HERMES_OPENAI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty());
        }
        if provider == "stepfun" {
            return std::env::var("HERMES_STEPFUN_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("STEPFUN_API_KEY").ok())
                .filter(|v| !v.trim().is_empty());
        }
        match provider {
            "google-ai-studio" => std::env::var("GOOGLE_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "gemini" | "google" | "google-gemini" => std::env::var("GEMINI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "gmi" | "gmi-cloud" | "gmicloud" => std::env::var("GMI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "arcee" | "arcee-ai" | "arceeai" => std::env::var("ARCEEAI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("ARCEE_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "xiaomi" | "mimo" | "xiaomi-mimo" => std::env::var("XIAOMI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "tencent-tokenhub" | "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => {
                std::env::var("TOKENHUB_API_KEY")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            }
            "anthropic" | "claude" | "claude-code" => std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("ANTHROPIC_TOKEN").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
                .filter(|v| !v.trim().is_empty()),
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                std::env::var("HERMES_GEMINI_OAUTH_API_KEY")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            }
            "openrouter" => std::env::var("OPENROUTER_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "qwen" | "qwen-oauth" => std::env::var("DASHSCOPE_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "kimi-coding" => std::env::var("KIMI_CODING_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("KIMI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("MOONSHOT_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "kimi" | "moonshot" => std::env::var("KIMI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("KIMI_CODING_API_KEY").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("MOONSHOT_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "kimi-coding-cn" => std::env::var("KIMI_CN_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("KIMI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("MOONSHOT_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "minimax" => std::env::var("MINIMAX_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            "minimax-cn" | "minimax_cn" => std::env::var("MINIMAX_CN_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("MINIMAX_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "nous" | "nous-api" | "nous_api" | "nousapi" | "nous-portal-api" => {
                std::env::var("NOUS_API_KEY")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            }
            "zai" | "glm" | "z-ai" | "z_ai" | "zhipu" => std::env::var("GLM_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("ZAI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("Z_AI_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            "copilot" | "copilot-acp" => std::env::var("COPILOT_GITHUB_TOKEN")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("GH_TOKEN").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("GITHUB_TOKEN").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| std::env::var("GITHUB_COPILOT_TOKEN").ok())
                .filter(|v| !v.trim().is_empty()),
            _ => None,
        }
    }

    fn resolve_runtime_base_url(
        &self,
        provider: &str,
        route_base_url: Option<&str>,
    ) -> Option<String> {
        if let Some(b) = route_base_url.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(b.to_string());
        }
        self.runtime_provider_config(provider)
            .and_then(|c| c.base_url.as_ref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                if provider == "copilot-acp" {
                    std::env::var("COPILOT_ACP_BASE_URL")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some("acp://copilot".to_string()))
                } else if provider == "openai-codex" || provider == "codex" {
                    Some(OPENAI_CODEX_BASE_URL.to_string())
                } else if provider == "qwen-oauth" {
                    Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string())
                } else if provider == "google-gemini-cli" {
                    Some("cloudcode-pa://google".to_string())
                } else if matches!(
                    provider,
                    "bedrock" | "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon"
                ) {
                    std::env::var("BEDROCK_BASE_URL")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some(bedrock_runtime_base_url(&resolve_bedrock_region())))
                } else if matches!(
                    provider,
                    "nous-api" | "nous_api" | "nousapi" | "nous-portal-api"
                ) {
                    std::env::var("NOUS_BASE_URL")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some("https://inference-api.nousresearch.com/v1".to_string()))
                } else if provider == "stepfun" {
                    Some("https://api.stepfun.ai/step_plan/v1".to_string())
                } else if matches!(provider, "kimi" | "moonshot" | "kimi-coding") {
                    std::env::var("KIMI_BASE_URL")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            Some(crate::provider_profiles::KIMI_LEGACY_BASE_URL.to_string())
                        })
                } else if provider == "kimi-coding-cn" {
                    Some(crate::provider_profiles::KIMI_CN_BASE_URL.to_string())
                } else if provider == "minimax-cn" || provider == "minimax_cn" {
                    Some("https://api.minimaxi.com/anthropic".to_string())
                } else if provider == "copilot" {
                    Some("https://api.githubcopilot.com".to_string())
                } else if matches!(
                    provider,
                    "gemini" | "google" | "google-gemini" | "google-ai-studio"
                ) {
                    Some("https://generativelanguage.googleapis.com/v1beta/openai".to_string())
                } else if matches!(provider, "gmi" | "gmi-cloud" | "gmicloud") {
                    Some("https://api.gmi-serving.com/v1".to_string())
                } else if matches!(provider, "arcee" | "arcee-ai" | "arceeai") {
                    Some("https://api.arcee.ai/api/v1".to_string())
                } else if matches!(provider, "xiaomi" | "mimo" | "xiaomi-mimo") {
                    Some("https://api.xiaomimimo.com/v1".to_string())
                } else if matches!(
                    provider,
                    "tencent-tokenhub" | "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas"
                ) {
                    Some("https://tokenhub.tencentmaas.com/v1".to_string())
                } else if matches!(provider, "zai" | "glm" | "z-ai" | "z_ai" | "zhipu") {
                    Some("https://api.z.ai/api/paas/v4".to_string())
                } else if crate::local_backends::is_local_backend_provider(provider) {
                    crate::local_backends::local_backend_resolved_base_url(provider)
                } else {
                    None
                }
            })
    }

    fn has_explicit_runtime_base_url(&self, provider: &str, route_base_url: Option<&str>) -> bool {
        if route_base_url
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        {
            return true;
        }
        if self
            .runtime_provider_config(provider)
            .and_then(|c| c.base_url.as_deref())
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        {
            return true;
        }
        if matches!(provider, "kimi" | "moonshot" | "kimi-coding") {
            return std::env::var("KIMI_BASE_URL")
                .ok()
                .map(|value| value.trim().to_string())
                .is_some_and(|value| !value.is_empty());
        }
        false
    }

    fn resolve_kimi_runtime_base_url_for_key(
        &self,
        provider: &str,
        route_base_url: Option<&str>,
        api_key: &str,
        base_url: Option<String>,
    ) -> Option<String> {
        if !matches!(provider, "kimi" | "moonshot" | "kimi-coding") {
            return base_url;
        }
        if self.has_explicit_runtime_base_url(provider, route_base_url) {
            return base_url;
        }
        if api_key.trim().starts_with("sk-kimi-") {
            Some(crate::provider_profiles::KIMI_CODE_BASE_URL.to_string())
        } else {
            base_url.or_else(|| Some(crate::provider_profiles::KIMI_LEGACY_BASE_URL.to_string()))
        }
    }

    fn resolve_runtime_request_timeout_seconds(&self, provider: &str) -> Option<f64> {
        self.runtime_provider_config(provider)
            .and_then(|c| c.request_timeout_seconds)
            .or_else(|| {
                let alias = match provider {
                    "codex" => "openai-codex",
                    "openai-codex" => "codex",
                    "qwen" => "qwen-oauth",
                    "qwen-oauth" => "qwen",
                    "kimi" => "moonshot",
                    "moonshot" => "kimi",
                    _ => return None,
                };
                self.config
                    .runtime_providers
                    .get(alias)
                    .and_then(|c| c.request_timeout_seconds)
            })
    }

    fn resolve_oauth_store_api_key(&self, provider: &str) -> Option<String> {
        let provider_key = match provider {
            "openai" => "openai",
            "openai-codex" | "codex" => "openai-codex",
            "nous" => "nous",
            "qwen-oauth" => "qwen-oauth",
            "anthropic" | "claude" | "claude-code" => "anthropic",
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => "google-gemini-cli",
            _ => return None,
        };
        let path = self.auth_tokens_path();
        let raw = std::fs::read_to_string(path).ok()?;
        let entries: HashMap<String, OAuthStoreCredential> = serde_json::from_str(&raw).ok()?;
        let cred = entries.get(provider_key)?;
        if cred.access_token.trim().is_empty() {
            return None;
        }
        if cred
            .expires_at
            .map(|exp| exp <= Utc::now())
            .unwrap_or(false)
        {
            return None;
        }
        Some(cred.access_token.clone())
    }

    async fn refresh_oauth_store_tokens_if_needed(&self) {
        // Keep this list explicit so behavior is deterministic and parity-scoped.
        self.refresh_single_oauth_store_token_if_needed("openai")
            .await;
        self.refresh_single_oauth_store_token_if_needed("openai-codex")
            .await;
        self.refresh_single_oauth_store_token_if_needed("nous")
            .await;
        self.refresh_single_oauth_store_token_if_needed("qwen-oauth")
            .await;
        self.refresh_single_oauth_store_token_if_needed("anthropic")
            .await;
    }

    async fn refresh_single_oauth_store_token_if_needed(&self, provider_key: &str) {
        if !self.can_attempt_oauth_refresh(provider_key) {
            return;
        }
        let path = self.auth_tokens_path();
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut entries: HashMap<String, OAuthStoreCredential> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return,
        };
        let Some(current) = entries.get(provider_key).cloned() else {
            return;
        };
        let Some(expires_at) = current.expires_at else {
            return;
        };
        if expires_at > Utc::now() {
            return;
        }
        let Some(refresh_token) = current.refresh_token.clone() else {
            return;
        };
        let Some((token_url, client_id)) = self.oauth_refresh_config(provider_key) else {
            return;
        };
        let refreshed = match self
            .exchange_oauth_refresh_token(
                provider_key,
                token_url.as_str(),
                client_id.as_str(),
                refresh_token.as_str(),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.mark_oauth_refresh_failure(provider_key);
                tracing::warn!(
                    provider = provider_key,
                    error = %e,
                    "oauth token refresh failed for runtime provider"
                );
                return;
            }
        };
        entries.insert(provider_key.to_string(), refreshed);
        let Ok(content) = serde_json::to_string_pretty(&entries) else {
            self.mark_oauth_refresh_success(provider_key);
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(path, content).await;
        self.mark_oauth_refresh_success(provider_key);
    }

    fn oauth_refresh_config(&self, provider_key: &str) -> Option<(String, String)> {
        // Preferred source: unified provider config centre (runtime_providers).
        let cfg_token_url = self
            .config
            .runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_token_url.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let cfg_client_id = self
            .config
            .runtime_providers
            .get(provider_key)
            .and_then(|c| c.oauth_client_id.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Env fallback — keeps previous behavior working when config centre is empty.
        let (token_url_env, client_id_env) = match provider_key {
            "openai" => (
                "HERMES_OPENAI_OAUTH_TOKEN_URL",
                "HERMES_OPENAI_OAUTH_CLIENT_ID",
            ),
            "openai-codex" => (
                "HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL",
                "HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID",
            ),
            "nous" => ("HERMES_NOUS_OAUTH_TOKEN_URL", "HERMES_NOUS_OAUTH_CLIENT_ID"),
            "qwen-oauth" => ("HERMES_QWEN_OAUTH_TOKEN_URL", "HERMES_QWEN_OAUTH_CLIENT_ID"),
            "anthropic" => (
                "HERMES_ANTHROPIC_OAUTH_TOKEN_URL",
                "HERMES_ANTHROPIC_OAUTH_CLIENT_ID",
            ),
            _ => return cfg_token_url.zip(cfg_client_id),
        };
        let token_url = cfg_token_url
            .or_else(|| {
                std::env::var(token_url_env)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| match provider_key {
                "openai" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("https://auth.openai.com/oauth/token".to_string())),
                "nous" => std::env::var("NOUS_PORTAL_BASE_URL")
                    .ok()
                    .map(|s| s.trim().trim_end_matches('/').to_string())
                    .filter(|s| !s.is_empty())
                    .map(|base| format!("{base}/api/oauth/token"))
                    .or_else(|| {
                        Some("https://portal.nousresearch.com/api/oauth/token".to_string())
                    }),
                "anthropic" => Some("https://console.anthropic.com/v1/oauth/token".to_string()),
                _ => None,
            })?;
        let client_id = cfg_client_id
            .or_else(|| {
                std::env::var(client_id_env)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| match provider_key {
                "openai" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("app_EMoamEEZ73f0CkXaXp7hrann".to_string())),
                "nous" => std::env::var("NOUS_CLIENT_ID")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some("hermes-cli".to_string())),
                "anthropic" => Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string()),
                _ => None,
            })?;
        Some((token_url, client_id))
    }

    async fn exchange_oauth_refresh_token(
        &self,
        provider_key: &str,
        token_url: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<OAuthStoreCredential, AgentError> {
        let endpoints = OAuth2Endpoints {
            authorize_url: "http://127.0.0.1/oauth/authorize-unused".to_string(),
            token_url: token_url.to_string(),
            client_id: client_id.to_string(),
            redirect_uri: "http://127.0.0.1/unused".to_string(),
            scopes: vec![],
        };
        let cred = exchange_refresh_token(provider_key, &endpoints, refresh_token)
            .await
            .map_err(|e| AgentError::AuthFailed(e.to_string()))?;
        Ok(OAuthStoreCredential {
            provider: Some(provider_key.to_string()),
            access_token: cred.access_token,
            refresh_token: cred
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            token_type: Some(cred.token_type),
            scope: cred.scope,
            expires_at: cred.expires_at,
        })
    }

    fn can_attempt_oauth_refresh(&self, provider_key: &str) -> bool {
        let Ok(guard) = self.oauth_refresh_backoff.lock() else {
            return true;
        };
        let Some(last_fail) = guard.get(provider_key) else {
            return true;
        };
        last_fail.elapsed().as_secs() >= OAUTH_REFRESH_BACKOFF_SECS
    }

    fn mark_oauth_refresh_failure(&self, provider_key: &str) {
        if let Ok(mut guard) = self.oauth_refresh_backoff.lock() {
            guard.insert(provider_key.to_string(), Instant::now());
        }
    }

    fn mark_oauth_refresh_success(&self, provider_key: &str) {
        if let Ok(mut guard) = self.oauth_refresh_backoff.lock() {
            guard.remove(provider_key);
        }
    }

    fn auth_tokens_path(&self) -> PathBuf {
        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home.join("auth").join("tokens.json")
    }

    fn objective_runtime_ledger_path(&self) -> PathBuf {
        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home
            .join("alpha")
            .join("objective_runtime_ledger.jsonl")
    }

    fn objective_eval_trend_path(&self) -> PathBuf {
        let hermes_home = self
            .config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
            .unwrap_or_else(|| PathBuf::from(".hermes"));
        hermes_home.join("alpha").join("objective_eval_trend.json")
    }

    fn append_objective_eval_sample(
        &self,
        objective_id: &str,
        objective_state: &str,
        note: &str,
    ) -> Result<(), AgentError> {
        let path = self.objective_eval_trend_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!("create {} failed: {}", parent.display(), e))
            })?;
        }
        let mut root: serde_json::Value = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .unwrap_or_else(
                    || serde_json::json!({"updated_at": Utc::now().to_rfc3339(), "samples": []}),
                )
        } else {
            serde_json::json!({"updated_at": Utc::now().to_rfc3339(), "samples": []})
        };
        let Some(samples) = root.get_mut("samples").and_then(|v| v.as_array_mut()) else {
            root = serde_json::json!({"updated_at": Utc::now().to_rfc3339(), "samples": []});
            let samples = root
                .get_mut("samples")
                .and_then(|v| v.as_array_mut())
                .ok_or_else(|| {
                    AgentError::Config("objective_eval_trend samples field missing".to_string())
                })?;
            samples.push(serde_json::json!({
                "recorded_at": Utc::now().to_rfc3339(),
                "objective_id": objective_id,
                "objective_state": objective_state,
                "score": objective_eval_score(objective_state),
                "note": note,
            }));
            root["updated_at"] = serde_json::json!(Utc::now().to_rfc3339());
            let payload = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
            std::fs::write(&path, payload)
                .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))?;
            return Ok(());
        };
        samples.push(serde_json::json!({
            "recorded_at": Utc::now().to_rfc3339(),
            "objective_id": objective_id,
            "objective_state": objective_state,
            "score": objective_eval_score(objective_state),
            "note": note,
        }));
        if samples.len() > 512 {
            let drain = samples.len().saturating_sub(512);
            samples.drain(0..drain);
        }
        root["updated_at"] = serde_json::json!(Utc::now().to_rfc3339());
        let payload = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(&path, payload)
            .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))?;
        Ok(())
    }

    fn append_objective_runtime_ledger(
        &self,
        messages: &[Message],
        assistant_text: &str,
        total_turns: u32,
    ) -> Result<(), AgentError> {
        let Some(objective) = extract_session_objective(messages) else {
            return Ok(());
        };
        if objective.trim().is_empty() {
            return Ok(());
        }
        let objective_id = short_sha256_hex(&format!("objective:{}", objective))
            .chars()
            .take(12)
            .collect::<String>();
        let objective_state = extract_objective_state_marker(assistant_text);
        let evidence_files = extract_marker_values(assistant_text, "path=", 12);
        let evidence_commands = extract_marker_values(assistant_text, "cmd=", 12);
        let decision = if objective_state == "advancing" {
            "promote"
        } else if objective_state == "regressing" {
            "investigate"
        } else if objective_state == "unproven" {
            "collect-more-evidence"
        } else {
            "monitor"
        };
        let entry = serde_json::json!({
            "recorded_at": Utc::now().to_rfc3339(),
            "objective_id": format!("obj-{}", objective_id),
            "objective_state": objective_state,
            "decision": decision,
            "turns": total_turns,
            "evidence_files": evidence_files,
            "evidence_commands": evidence_commands,
        });
        let path = self.objective_runtime_ledger_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!("create {} failed: {}", parent.display(), e))
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| AgentError::Io(format!("open {} failed: {}", path.display(), e)))?;
        writeln!(file, "{}", entry)
            .map_err(|e| AgentError::Io(format!("append {} failed: {}", path.display(), e)))?;
        self.append_objective_eval_sample(
            &format!("obj-{}", objective_id),
            &objective_state,
            &format!("decision={decision} turns={total_turns}"),
        )?;
        Ok(())
    }

    fn resolve_runtime_command_args(
        &self,
        provider: Option<&str>,
    ) -> (Option<String>, Vec<String>) {
        let mut command = self
            .config
            .acp_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mut args: Vec<String> = self
            .config
            .acp_args
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();

        if let Some(provider) = provider {
            if let Some(cfg) = self.runtime_provider_config(provider) {
                if let Some(cmd) = cfg
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    command = Some(cmd.to_string());
                }
                if !cfg.args.is_empty() {
                    args = cfg
                        .args
                        .iter()
                        .map(|a| a.trim().to_string())
                        .filter(|a| !a.is_empty())
                        .collect();
                }
            }
            if provider == "copilot-acp" {
                if command.is_none() {
                    command = std::env::var("HERMES_COPILOT_ACP_COMMAND")
                        .ok()
                        .or_else(|| std::env::var("COPILOT_CLI_PATH").ok())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| Some("copilot".to_string()));
                }
                if args.is_empty() {
                    args = std::env::var("HERMES_COPILOT_ACP_ARGS")
                        .ok()
                        .and_then(|raw| shlex::split(raw.trim()))
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| vec!["--acp".to_string(), "--stdio".to_string()]);
                }
                if let Some(cmd) = command.as_deref() {
                    if let Ok(resolved) = which::which(cmd) {
                        command = Some(resolved.to_string_lossy().to_string());
                    }
                }
            }
        }
        (command, args)
    }

    pub(crate) fn build_runtime_provider(
        &self,
        provider: &str,
        model_name: &str,
        route_base_url: Option<&str>,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
        api_mode: Option<&ApiMode>,
        credential_pool: Option<&Arc<CredentialPool>>,
    ) -> Result<Arc<dyn LlmProvider>, AgentError> {
        let base_url = self.resolve_runtime_base_url(provider, route_base_url);
        let api_key =
            match self.resolve_runtime_api_key(provider, api_key_env_override, explicit_api_key) {
                Some(api_key) => api_key,
                None if runtime_provider_allows_no_api_key(provider, base_url.as_deref()) => {
                    "local-no-key".to_string()
                }
                None => {
                    return Err(AgentError::Config(format!(
                        "No API key configured for runtime-routed provider '{}'",
                        provider
                    )))
                }
            };
        let base_url = self.resolve_kimi_runtime_base_url_for_key(
            provider,
            route_base_url,
            &api_key,
            base_url,
        );
        let request_timeout_seconds = self.resolve_runtime_request_timeout_seconds(provider);
        let use_openai_pro_backend = matches!(provider, "openai-codex" | "codex")
            || (provider == "openai" && is_codex_chatgpt_token(&api_key));
        let mode = if use_openai_pro_backend {
            &ApiMode::CodexResponses
        } else {
            api_mode.unwrap_or(&self.config.api_mode)
        };
        let normalized_model_name =
            crate::model_normalize::normalize_model_for_provider(model_name, provider);
        let wire_model_name =
            Self::runtime_wire_model_for_provider(provider, normalized_model_name.as_str());
        let model_name = wire_model_name.as_str();

        let provider_obj: Arc<dyn LlmProvider> = match provider {
            "openai" | "codex" | "openai-codex" => {
                if matches!(mode, ApiMode::CodexResponses) {
                    let mut p = if use_openai_pro_backend {
                        CodexProvider::openai_pro(&api_key, model_name)
                    } else {
                        CodexProvider::new(&api_key).with_model(model_name)
                    }
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                    if let Some(ref url) = base_url {
                        p = p.with_base_url(url.clone());
                    }
                    if let Some(pool) = credential_pool {
                        p = p.with_credential_pool(pool.clone());
                    }
                    Arc::new(p)
                } else {
                    let mut p = OpenAiProvider::new(&api_key)
                        .with_model(model_name)
                        .with_optional_request_timeout_seconds(request_timeout_seconds);
                    if let Some(url) = base_url {
                        p = p.with_base_url(url);
                    }
                    if let Some(pool) = credential_pool {
                        p = p.with_credential_pool(pool.clone());
                    }
                    Arc::new(p)
                }
            }
            "anthropic" => {
                let mut p = AnthropicProvider::new(&api_key)
                    .with_model(model_name)
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                if let Some(pool) = credential_pool {
                    p = p.with_credential_pool(pool.clone());
                }
                Arc::new(p)
            }
            "openrouter" => {
                let mut p = OpenRouterProvider::new(&api_key)
                    .with_model(model_name)
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(pool) = credential_pool {
                    p = p.with_credential_pool(pool.clone());
                }
                Arc::new(p)
            }
            "qwen" | "qwen-oauth" => {
                let mut p = QwenProvider::new(&api_key)
                    .with_model(model_name)
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "kimi" | "moonshot" | "kimi-coding" => {
                let mut p = KimiProvider::new(&api_key)
                    .with_model(model_name)
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "minimax" => {
                let mut p = MiniMaxProvider::new(&api_key)
                    .with_model(model_name)
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "stepfun" => {
                let url =
                    base_url.unwrap_or_else(|| "https://api.stepfun.ai/step_plan/v1".to_string());
                Arc::new(
                    GenericProvider::new(url, &api_key, model_name)
                        .with_optional_request_timeout_seconds(request_timeout_seconds)
                        .with_provider_profile(provider),
                )
            }
            "nous" | "nous-api" | "nous_api" | "nousapi" | "nous-portal-api" => {
                let mut p = NousProvider::new(&api_key)
                    .with_model(model_name)
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "bedrock" | "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon" => {
                let mut p = BedrockProvider::new()
                    .with_region(resolve_bedrock_region())
                    .with_model(model_name);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
            "copilot" | "copilot-acp" => {
                let p = CopilotProvider::new(
                    base_url.unwrap_or_else(|| "https://api.githubcopilot.com".to_string()),
                    &api_key,
                )
                .with_model(model_name)
                .with_optional_request_timeout_seconds(request_timeout_seconds);
                Arc::new(p)
            }
            _ => {
                let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                let mut g = GenericProvider::new(url, &api_key, model_name)
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(provider);
                if let Some(pool) = credential_pool {
                    g = g.with_credential_pool(pool.clone());
                }
                Arc::new(g)
            }
        };
        Ok(provider_obj)
    }

    pub(crate) fn build_delegation_runtime_provider(
        &self,
        provider: &str,
        model_name: &str,
        route_base_url: Option<&str>,
        explicit_api_key: Option<&str>,
    ) -> Result<Arc<dyn LlmProvider>, AgentError> {
        let api_mode = self
            .runtime_provider_config(provider)
            .and_then(|cfg| cfg.api_mode.clone())
            .or_else(|| route_base_url.and_then(detect_api_mode_for_url));
        self.build_runtime_provider(
            provider,
            model_name,
            route_base_url,
            None,
            explicit_api_key,
            api_mode.as_ref(),
            self.primary_credential_pool.as_ref(),
        )
    }

}
