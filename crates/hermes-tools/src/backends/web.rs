//! Real web tool backends: Exa search, Firecrawl extract, and local fallbacks.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::web::{WebExtractBackend, WebSearchBackend};
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use hermes_core::ToolError;

// ---------------------------------------------------------------------------
// FallbackSearchBackend (no API key needed)
// ---------------------------------------------------------------------------

/// A search backend that returns a helpful message when no API key is configured.
pub struct FallbackSearchBackend;

impl FallbackSearchBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FallbackSearchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebSearchBackend for FallbackSearchBackend {
    async fn search(
        &self,
        query: &str,
        _num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        Ok(json!({
            "error": "no_api_key",
            "message": format!(
                "Web search is not configured. To enable, set one of the following environment variables:\n\
                 - EXA_API_KEY (https://exa.ai)\n\
                 - TAVILY_API_KEY (https://tavily.com)\n\
                 - SERPER_API_KEY (https://serper.dev)\n\n\
                 Query was: {}", query
            ),
            "query": query,
        }).to_string())
    }
}

// ---------------------------------------------------------------------------
// SimpleExtractBackend (uses reqwest, no API key needed)
// ---------------------------------------------------------------------------

const MAX_EXTRACT_BYTES: usize = 512_000; // 500 KB
const TAVILY_BASE_URL_DEFAULT: &str = "https://api.tavily.com";

/// A web extraction backend that fetches HTML via reqwest with no external API dependency.
pub struct SimpleExtractBackend {
    client: Client,
}

impl SimpleExtractBackend {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("hermes-agent/1.0")
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

impl Default for SimpleExtractBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebExtractBackend for SimpleExtractBackend {
    async fn extract(&self, url: &str, _include_links: bool) -> Result<String, ToolError> {
        let resp =
            self.client.get(url).send().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to fetch '{}': {}", url, e))
            })?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HTTP {} when fetching '{}'",
                status, url
            )));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = resp.bytes().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read response body: {}", e))
        })?;

        if bytes.len() > MAX_EXTRACT_BYTES {
            let text = String::from_utf8_lossy(&bytes[..MAX_EXTRACT_BYTES]);
            let result = json!({
                "url": url,
                "content_type": content_type,
                "content": text,
                "truncated": true,
                "original_size": bytes.len(),
            });
            return serde_json::to_string_pretty(&result)
                .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {}", e)));
        }

        let text = String::from_utf8_lossy(&bytes);

        let content = strip_html_tags(&text);

        let result = json!({
            "url": url,
            "content_type": content_type,
            "content": content,
            "truncated": false,
            "size": bytes.len(),
        });

        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {}", e)))
    }
}

/// Minimal HTML tag stripper for producing readable text from HTML.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_space = false;

    let lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        if !in_tag && chars[i] == '<' {
            in_tag = true;
            let remaining: String = lower_chars[i..].iter().take(20).collect();
            if remaining.starts_with("<script") {
                in_script = true;
            } else if remaining.starts_with("</script") {
                in_script = false;
            } else if remaining.starts_with("<style") {
                in_style = true;
            } else if remaining.starts_with("</style") {
                in_style = false;
            }
            i += 1;
            continue;
        }

        if in_tag {
            if chars[i] == '>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }

        if in_script || in_style {
            i += 1;
            continue;
        }

        let ch = chars[i];
        if ch.is_whitespace() {
            if !last_was_space && !result.is_empty() {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }
        i += 1;
    }

    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// ExaSearchBackend
// ---------------------------------------------------------------------------

/// Real Exa API search backend.
pub struct ExaSearchBackend {
    client: Client,
    api_key: String,
}

impl ExaSearchBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }

    /// Create from environment variable `EXA_API_KEY`.
    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = std::env::var("EXA_API_KEY").map_err(|_| {
            ToolError::ExecutionFailed("EXA_API_KEY environment variable not set".into())
        })?;
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "EXA_API_KEY environment variable is empty".into(),
            ));
        }
        Ok(Self::new(api_key.to_string()))
    }
}

#[async_trait]
impl WebSearchBackend for ExaSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut body = json!({
            "query": query,
            "numResults": num_results,
            "type": "auto",
            "contents": {
                "text": true
            }
        });

        if let Some(cat) = category {
            body["category"] = json!(cat);
        }

        let resp = self
            .client
            .post("https://api.exa.ai/search")
            .header("x-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Exa API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Exa response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Exa API error ({}): {}",
                status, text
            )));
        }

        // Parse and reformat the response
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Exa response: {}", e))
        })?;

        let results = data.get("results").and_then(|r| r.as_array());
        let formatted: Vec<Value> = results
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        json!({
                            "title": r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                            "url": r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                            "text": r.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                            "score": r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        serde_json::to_string_pretty(&json!({ "results": formatted }))
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// TavilySearchBackend
// ---------------------------------------------------------------------------

/// Real Tavily API search backend.
pub struct TavilySearchBackend {
    client: Client,
    api_key: String,
    base_url: String,
}

impl TavilySearchBackend {
    pub fn new(api_key: String, base_url: String) -> Self {
        let normalized_base = base_url.trim().trim_end_matches('/').to_string();
        let base_url = if normalized_base.is_empty() {
            TAVILY_BASE_URL_DEFAULT.to_string()
        } else {
            normalized_base
        };
        Self {
            client: Client::new(),
            api_key,
            base_url,
        }
    }

    /// Create from environment variables:
    /// - `TAVILY_API_KEY` (required)
    /// - `TAVILY_BASE_URL` (optional, defaults to `https://api.tavily.com`)
    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = std::env::var("TAVILY_API_KEY").map_err(|_| {
            ToolError::ExecutionFailed("TAVILY_API_KEY environment variable not set".into())
        })?;
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "TAVILY_API_KEY environment variable is empty".into(),
            ));
        }
        let base_url = std::env::var("TAVILY_BASE_URL")
            .unwrap_or_else(|_| TAVILY_BASE_URL_DEFAULT.to_string());
        Ok(Self::new(api_key.to_string(), base_url))
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait]
impl WebSearchBackend for TavilySearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
    ) -> Result<String, ToolError> {
        let topic = match category.unwrap_or("").trim().to_lowercase().as_str() {
            "news" => "news",
            _ => "general",
        };
        let body = json!({
            "api_key": self.api_key,
            "query": query,
            "max_results": num_results,
            "topic": topic,
            "search_depth": "basic",
            "include_answer": false,
            "include_images": false,
            "include_raw_content": false,
        });

        let endpoint = format!("{}/search", self.base_url);
        let resp = self
            .client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Tavily API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Tavily response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Tavily API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Tavily response: {}", e))
        })?;

        let results = data.get("results").and_then(|r| r.as_array());
        let formatted: Vec<Value> = results
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        json!({
                            "title": r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                            "url": r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                            "text": r
                                .get("content")
                                .or_else(|| r.get("raw_content"))
                                .and_then(|v| v.as_str())
                                .unwrap_or(""),
                            "score": r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        serde_json::to_string_pretty(&json!({ "results": formatted }))
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {}", e)))
    }
}

/// Resolve preferred web-search backend from environment.
///
/// Priority:
/// 1. Exa (`EXA_API_KEY`)
/// 2. Tavily (`TAVILY_API_KEY`, optional `TAVILY_BASE_URL`)
/// 3. Fallback helpful message backend
pub fn search_backend_from_env_or_fallback() -> Box<dyn WebSearchBackend> {
    match search_backend_choice_from_env() {
        "exa" => ExaSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "tavily" => TavilySearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        _ => Box::new(FallbackSearchBackend::new()),
    }
}

fn search_backend_choice_from_env() -> &'static str {
    if std::env::var("EXA_API_KEY")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        "exa"
    } else if std::env::var("TAVILY_API_KEY")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        "tavily"
    } else {
        "fallback"
    }
}

// ---------------------------------------------------------------------------
// FirecrawlExtractBackend
// ---------------------------------------------------------------------------

/// Identifies how a Firecrawl request reaches the API. Reflected in the
/// returned JSON's `transport` field for observability.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FirecrawlTransport {
    /// Direct call to `https://api.firecrawl.dev/v1/...` with the user's
    /// `FIRECRAWL_API_KEY`.
    Direct { api_key: String },
    /// Routed through a Nous-managed gateway with a Nous OAuth bearer.
    Managed {
        endpoint_root: String,
        nous_token: String,
    },
}

impl FirecrawlTransport {
    fn label(&self) -> &'static str {
        match self {
            Self::Direct { .. } => "direct",
            Self::Managed { .. } => "managed",
        }
    }

    fn scrape_endpoint(&self) -> String {
        match self {
            Self::Direct { .. } => "https://api.firecrawl.dev/v1/scrape".into(),
            Self::Managed { endpoint_root, .. } => format!("{endpoint_root}/v1/scrape"),
        }
    }

    fn bearer(&self) -> &str {
        match self {
            Self::Direct { api_key } => api_key,
            Self::Managed { nous_token, .. } => nous_token,
        }
    }
}

/// Real Firecrawl API extract backend.
///
/// Resolution order at construction time:
///
/// 1. Direct: `FIRECRAWL_API_KEY` env var → calls firecrawl.dev directly.
/// 2. Managed: when (1) is missing AND `HERMES_ENABLE_NOUS_MANAGED_TOOLS`
///    is on with a Nous access token, the call is routed through the
///    `firecrawl` vendor gateway.
///
/// `transport` is reflected in the returned JSON so callers can audit
/// where the request actually went.
#[derive(Debug)]
pub struct FirecrawlExtractBackend {
    client: Client,
    transport: FirecrawlTransport,
}

impl FirecrawlExtractBackend {
    /// Construct a direct backend from an explicit API key.
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            transport: FirecrawlTransport::Direct { api_key },
        }
    }

    /// Construct a managed-mode backend from a resolved gateway config.
    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            client: Client::new(),
            transport: FirecrawlTransport::Managed {
                endpoint_root: cfg.gateway_origin.trim_end_matches('/').to_string(),
                nous_token: cfg.nous_user_token.clone(),
            },
        }
    }

    /// Resolve the best-available transport.
    ///
    /// Priority: direct `FIRECRAWL_API_KEY` → Nous-managed `firecrawl`
    /// vendor → `Err` with a hint covering both paths.
    pub fn from_env_or_managed() -> Result<Self, ToolError> {
        if let Ok(api_key) = std::env::var("FIRECRAWL_API_KEY") {
            let trimmed = api_key.trim();
            if !trimmed.is_empty() {
                return Ok(Self::new(trimmed.to_string()));
            }
        }
        if let Some(cfg) = resolve_managed_tool_gateway("firecrawl", ResolveOptions::default()) {
            return Ok(Self::from_managed(&cfg));
        }
        Err(ToolError::ExecutionFailed(
            "FIRECRAWL_API_KEY not set and Nous-managed firecrawl gateway is not configured."
                .into(),
        ))
    }

    /// Backwards-compatible alias of [`from_env_or_managed`]. Kept for any
    /// existing callers that still call `from_env()`.
    pub fn from_env() -> Result<Self, ToolError> {
        Self::from_env_or_managed()
    }

    /// Reports the active transport. Useful for tests/logging.
    pub fn transport_label(&self) -> &'static str {
        self.transport.label()
    }
}

#[async_trait]
impl WebExtractBackend for FirecrawlExtractBackend {
    async fn extract(&self, url: &str, include_links: bool) -> Result<String, ToolError> {
        let body = json!({
            "url": url,
            "formats": ["markdown"],
            "onlyMainContent": true,
            "includeLinks": include_links,
        });

        let resp = self
            .client
            .post(self.transport.scrape_endpoint())
            .header(
                "Authorization",
                format!("Bearer {}", self.transport.bearer()),
            )
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Firecrawl API request failed: {}", e))
            })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Firecrawl response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Firecrawl API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Firecrawl response: {}", e))
        })?;

        let markdown = data
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");

        let metadata = data
            .get("data")
            .and_then(|d| d.get("metadata"))
            .cloned()
            .unwrap_or(json!({}));

        let links = data
            .get("data")
            .and_then(|d| d.get("links"))
            .cloned()
            .unwrap_or(json!([]));

        let result = json!({
            "content": markdown,
            "metadata": metadata,
            "links": links,
            "transport": self.transport.label(),
        });

        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize result: {}", e)))
    }
}

#[cfg(test)]
mod web_search_env_tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    struct EnvScope {
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let keys = ["EXA_API_KEY", "TAVILY_API_KEY", "TAVILY_BASE_URL"];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                std::env::remove_var(k);
            }
            Self { original, _g: g }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn tavily_from_env_defaults_base_url() {
        let _scope = EnvScope::new();
        std::env::set_var("TAVILY_API_KEY", "tavily-key");
        let backend = TavilySearchBackend::from_env().expect("tavily backend from env");
        assert_eq!(backend.base_url(), TAVILY_BASE_URL_DEFAULT);
    }

    #[test]
    fn tavily_from_env_honors_custom_base_url() {
        let _scope = EnvScope::new();
        std::env::set_var("TAVILY_API_KEY", "tavily-key");
        std::env::set_var("TAVILY_BASE_URL", "https://proxy.example.com/tavily/");
        let backend = TavilySearchBackend::from_env().expect("tavily backend from env");
        assert_eq!(backend.base_url(), "https://proxy.example.com/tavily");
    }

    #[test]
    fn search_backend_choice_prefers_exa_over_tavily() {
        let _scope = EnvScope::new();
        std::env::set_var("EXA_API_KEY", "exa-key");
        std::env::set_var("TAVILY_API_KEY", "tavily-key");
        assert_eq!(search_backend_choice_from_env(), "exa");
    }

    #[test]
    fn search_backend_choice_uses_tavily_when_exa_missing() {
        let _scope = EnvScope::new();
        std::env::set_var("TAVILY_API_KEY", "tavily-key");
        assert_eq!(search_backend_choice_from_env(), "tavily");
    }

    #[tokio::test]
    async fn search_backend_falls_back_when_keys_missing() {
        let _scope = EnvScope::new();
        let backend = search_backend_from_env_or_fallback();
        let out = backend
            .search("hello", 3, None)
            .await
            .expect("fallback backend should return json");
        assert!(out.contains("\"no_api_key\""));
    }
}

#[cfg(test)]
mod firecrawl_managed_tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    /// Hermetic env scope: HERMES_HOME → tempdir + flag/token cleared.
    struct EnvScope {
        _tmp: tempfile::TempDir,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMES_HOME",
                "FIRECRAWL_API_KEY",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                std::env::remove_var(k);
            }
            std::env::set_var("HERMES_HOME", tmp.path());
            Self {
                _tmp: tmp,
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn from_env_or_managed_prefers_direct_key() {
        let _g = EnvScope::new();
        std::env::set_var("FIRECRAWL_API_KEY", "direct-key");
        let b = FirecrawlExtractBackend::from_env_or_managed().unwrap();
        assert_eq!(b.transport_label(), "direct");
    }

    #[test]
    fn from_env_or_managed_falls_back_to_nous_gateway() {
        let _g = EnvScope::new();
        std::env::remove_var("FIRECRAWL_API_KEY");
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-tok");
        let b = FirecrawlExtractBackend::from_env_or_managed().unwrap();
        assert_eq!(b.transport_label(), "managed");
    }

    #[test]
    fn from_env_or_managed_errors_when_neither_configured() {
        let _g = EnvScope::new();
        let err = FirecrawlExtractBackend::from_env_or_managed().unwrap_err();
        assert!(err.to_string().contains("FIRECRAWL_API_KEY"));
        assert!(err.to_string().contains("firecrawl gateway"));
    }

    #[test]
    fn from_managed_uses_resolved_origin_and_token() {
        let cfg = ManagedToolGatewayConfig {
            vendor: "firecrawl".into(),
            gateway_origin: "https://firecrawl.gw.example.com/".into(),
            nous_user_token: "tok".into(),
            managed_mode: true,
        };
        let b = FirecrawlExtractBackend::from_managed(&cfg);
        match &b.transport {
            FirecrawlTransport::Managed {
                endpoint_root,
                nous_token,
            } => {
                assert_eq!(endpoint_root, "https://firecrawl.gw.example.com");
                assert_eq!(nous_token, "tok");
                assert_eq!(
                    b.transport.scrape_endpoint(),
                    "https://firecrawl.gw.example.com/v1/scrape"
                );
            }
            _ => panic!("expected managed transport"),
        }
    }

    #[test]
    fn empty_direct_key_falls_through_to_managed_fallback_or_error() {
        let _g = EnvScope::new();
        std::env::set_var("FIRECRAWL_API_KEY", "   ");
        // No managed config either → expect Err.
        let err = FirecrawlExtractBackend::from_env_or_managed().unwrap_err();
        assert!(err.to_string().contains("FIRECRAWL_API_KEY"));
    }
}
