#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserbaseConfig {
    api_key: String,
    project_id: String,
    base_url: String,
    proxies: bool,
    advanced_stealth: bool,
    keep_alive: bool,
    session_timeout_secs: Option<u64>,
    task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserbaseFeatures {
    basic_stealth: bool,
    proxies: bool,
    advanced_stealth: bool,
    keep_alive: bool,
    custom_timeout: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserbaseSession {
    session_name: String,
    bb_session_id: String,
    cdp_url: String,
    features: BrowserbaseFeatures,
}

/// Browserbase cloud browser backend.
///
/// Direct Browserbase credentials only. Nous-managed browser routing belongs to
/// Browser Use, matching the current provider contract.
pub struct BrowserbaseBrowserBackend {
    config: BrowserbaseConfig,
    client: reqwest::Client,
    session: Mutex<Option<BrowserbaseSession>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserUseConfig {
    api_key: String,
    base_url: String,
    managed_mode: bool,
    task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserUseSession {
    session_name: String,
    bb_session_id: String,
    cdp_url: String,
    external_call_id: Option<String>,
}

/// Browser Use cloud browser backend.
///
/// Supports both direct `BROWSER_USE_API_KEY` and the Nous-managed Browser Use
/// gateway. Managed mode preserves upstream's idempotency-key behavior for
/// in-flight or retryable session creation.
pub struct BrowserUseBrowserBackend {
    config: BrowserUseConfig,
    client: reqwest::Client,
    session: Mutex<Option<BrowserUseSession>>,
    pending_create_key: Mutex<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirecrawlBrowserConfig {
    api_key: String,
    base_url: String,
    ttl_secs: u64,
    task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirecrawlBrowserSession {
    session_name: String,
    bb_session_id: String,
    cdp_url: String,
    ttl_secs: u64,
}

/// Firecrawl cloud browser backend.
///
/// This is distinct from the Firecrawl web search/extract backend: it creates
/// remote CDP browser sessions via `/v2/browser` and then routes browser tool
/// commands to the returned CDP URL.
pub struct FirecrawlBrowserBackend {
    config: FirecrawlBrowserConfig,
    client: reqwest::Client,
    session: Mutex<Option<FirecrawlBrowserSession>>,
}

impl UnavailableBrowserBackend {
    fn new(message: String) -> Self {
        Self { message }
    }

    fn unavailable(&self) -> ToolError {
        ToolError::ExecutionFailed(self.message.clone())
    }
}

impl CloudFallbackBrowserBackend {
    fn new(provider: &'static str, cloud: Arc<dyn BrowserBackend>) -> Self {
        Self {
            provider,
            cloud,
            local: CdpBrowserBackend::from_env(),
        }
    }

    fn fallback_error(&self, cloud_err: &ToolError, local_err: ToolError) -> ToolError {
        ToolError::ExecutionFailed(format!(
            "{} failed: {}; local CDP fallback failed: {}",
            self.provider, cloud_err, local_err
        ))
    }

    fn mark_fallback(&self, local_result: String, cloud_err: &ToolError) -> String {
        browser_fallback_response(local_result, self.provider, &cloud_err.to_string())
    }
}

impl BrowserbaseConfig {
    pub fn new(api_key: String, project_id: String) -> Self {
        Self {
            api_key,
            project_id,
            base_url: BROWSERBASE_BASE_URL_DEFAULT.to_string(),
            proxies: true,
            advanced_stealth: false,
            keep_alive: true,
            session_timeout_secs: None,
            task_id: "rust".to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = env_optional_nonempty("BROWSERBASE_API_KEY").ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Browserbase requires BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID".into(),
            )
        })?;
        let project_id = env_optional_nonempty("BROWSERBASE_PROJECT_ID").ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Browserbase requires BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID".into(),
            )
        })?;
        let mut cfg = Self::new(api_key, project_id);
        if let Some(base_url) = env_optional_nonempty("BROWSERBASE_BASE_URL") {
            cfg.base_url = normalize_base_url(&base_url);
        }
        cfg.proxies = env_bool("BROWSERBASE_PROXIES", true);
        cfg.advanced_stealth = env_bool("BROWSERBASE_ADVANCED_STEALTH", false);
        cfg.keep_alive = env_bool("BROWSERBASE_KEEP_ALIVE", true);
        cfg.session_timeout_secs = env_optional_nonempty("BROWSERBASE_SESSION_TIMEOUT")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .map(|v| v.min(BROWSERBASE_MAX_SESSION_TIMEOUT_SECS));
        if let Some(task_id) = env_optional_nonempty("HERMES_TASK_ID") {
            cfg.task_id = task_id;
        }
        Ok(cfg)
    }

    pub fn is_configured_from_env() -> bool {
        env_optional_nonempty("BROWSERBASE_API_KEY").is_some()
            && env_optional_nonempty("BROWSERBASE_PROJECT_ID").is_some()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl BrowserUseConfig {
    pub fn new_direct(api_key: String) -> Self {
        Self {
            api_key,
            base_url: BROWSER_USE_BASE_URL_DEFAULT.to_string(),
            managed_mode: false,
            task_id: "rust".to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let direct_api_key = env_optional_nonempty("BROWSER_USE_API_KEY");
        if let Some(api_key) = direct_api_key {
            if !browser_use_prefers_gateway_from_config() {
                let mut cfg = Self::new_direct(api_key);
                if let Some(task_id) = env_optional_nonempty("HERMES_TASK_ID") {
                    cfg.task_id = task_id;
                }
                return Ok(cfg);
            }
        }

        if let Some(managed) =
            resolve_managed_tool_gateway("browser-use", ResolveOptions::default())
        {
            let mut cfg = Self::from_managed(&managed);
            if let Some(task_id) = env_optional_nonempty("HERMES_TASK_ID") {
                cfg.task_id = task_id;
            }
            return Ok(cfg);
        }

        let message = if managed_nous_tools_enabled() {
            "Browser Use requires either a direct BROWSER_USE_API_KEY credential or a managed Browser Use gateway configuration."
        } else {
            "Browser Use requires a direct BROWSER_USE_API_KEY credential."
        };
        Err(ToolError::ExecutionFailed(message.into()))
    }

    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            api_key: cfg.nous_user_token.clone(),
            base_url: normalize_base_url_with_default(
                &cfg.gateway_origin,
                BROWSER_USE_BASE_URL_DEFAULT,
            ),
            managed_mode: true,
            task_id: "rust".to_string(),
        }
    }

    pub fn is_configured_from_env_or_managed() -> bool {
        env_optional_nonempty("BROWSER_USE_API_KEY").is_some()
            || is_managed_tool_gateway_ready("browser-use", ResolveOptions::default())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn managed_mode(&self) -> bool {
        self.managed_mode
    }
}

impl FirecrawlBrowserConfig {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: FIRECRAWL_BROWSER_BASE_URL_DEFAULT.to_string(),
            ttl_secs: FIRECRAWL_BROWSER_DEFAULT_TTL_SECS,
            task_id: "rust".to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = env_optional_nonempty("FIRECRAWL_API_KEY").ok_or_else(|| {
            ToolError::ExecutionFailed("Firecrawl browser requires FIRECRAWL_API_KEY.".into())
        })?;
        let mut cfg = Self::new(api_key);
        if let Some(base_url) = env_optional_nonempty("FIRECRAWL_API_URL") {
            cfg.base_url =
                normalize_base_url_with_default(&base_url, FIRECRAWL_BROWSER_BASE_URL_DEFAULT);
        }
        cfg.ttl_secs = env_optional_nonempty("FIRECRAWL_BROWSER_TTL")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(FIRECRAWL_BROWSER_DEFAULT_TTL_SECS);
        if let Some(task_id) = env_optional_nonempty("HERMES_TASK_ID") {
            cfg.task_id = task_id;
        }
        Ok(cfg)
    }

    pub fn is_configured_from_env() -> bool {
        env_optional_nonempty("FIRECRAWL_API_KEY").is_some()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
