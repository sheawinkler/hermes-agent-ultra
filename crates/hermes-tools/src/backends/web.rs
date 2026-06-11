//! Real web tool backends: Exa/Tavily/Firecrawl/xAI/SearXNG/Brave/DDG search,
//! Firecrawl/Tavily extract, Tavily crawl, and local fallbacks.

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::OnceLock;
use url::Url;
use uuid::Uuid;

use crate::tools::web::{WebCrawlBackend, WebExtractBackend, WebSearchBackend};
use hermes_config::managed_gateway::{
    is_managed_tool_gateway_ready, resolve_managed_tool_gateway, ManagedToolGatewayConfig,
    ResolveOptions,
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
                 - FIRECRAWL_API_KEY or FIRECRAWL_API_URL (https://firecrawl.dev)\n\
                 - PARALLEL_API_KEY with HERMES_WEB_SEARCH_BACKEND=parallel (https://parallel.ai)\n\
                 - HERMES_WEB_SEARCH_BACKEND=parallel for keyless Parallel Search MCP\n\
                 - XAI_API_KEY with HERMES_WEB_SEARCH_BACKEND=xai (https://x.ai)\n\
                 - SERPER_API_KEY (https://serper.dev)\n\
                 - SEARXNG_BASE_URL or SEARXNG_URL (https://docs.searxng.org/dev/search_api.html)\n\
                 - BRAVE_SEARCH_API_KEY with HERMES_WEB_SEARCH_BACKEND=brave-free\n\
                 - HERMES_WEB_SEARCH_BACKEND=ddgs for keyless DuckDuckGo Instant Answer search\n\n\
                 Query was: {}", query
            ),
            "query": query,
        }).to_string())
    }
}

/// A crawl backend that returns a helpful message when no crawl provider is configured.
pub struct FallbackCrawlBackend;

impl FallbackCrawlBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FallbackCrawlBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebCrawlBackend for FallbackCrawlBackend {
    async fn crawl(
        &self,
        url: &str,
        _instructions: Option<&str>,
        _depth: &str,
        _limit: usize,
    ) -> Result<String, ToolError> {
        Ok(json!({
            "success": false,
            "error": "no_api_key",
            "message": "Web crawl is not configured. To enable Tavily crawl, set TAVILY_API_KEY.",
            "results": [{
                "url": url,
                "title": "",
                "content": "",
                "error": "TAVILY_API_KEY environment variable not set"
            }]
        })
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// SimpleExtractBackend (uses reqwest, no API key needed)
// ---------------------------------------------------------------------------

const MAX_EXTRACT_BYTES: usize = 512_000; // 500 KB
const TAVILY_BASE_URL_DEFAULT: &str = "https://api.tavily.com";
const SEARXNG_SEARCH_PATH: &str = "/search";
const BRAVE_SEARCH_URL_DEFAULT: &str = "https://api.search.brave.com/res/v1/web/search";
const DDG_INSTANT_ANSWER_URL_DEFAULT: &str = "https://api.duckduckgo.com/";
const PARALLEL_BASE_URL_DEFAULT: &str = "https://api.parallel.ai";
const PARALLEL_MCP_SEARCH_URL_DEFAULT: &str = "https://search.parallel.ai/mcp";
const PARALLEL_MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const PARALLEL_MCP_CLIENT_NAME: &str = "mcp-web-client";
const PARALLEL_MCP_CLIENT_VERSION: &str = "1.0.0";
const PARALLEL_MCP_USER_AGENT: &str = "mcp-web-client/1.0.0";
const PARALLEL_FREE_SEARCH_ATTRIBUTION: &str =
    "Search powered by the free Parallel Web Search MCP (https://parallel.ai).";
const PARALLEL_FREE_EXTRACT_ATTRIBUTION: &str =
    "Extraction powered by the free Parallel Web Search MCP (https://parallel.ai).";
const FIRECRAWL_BASE_URL_DEFAULT: &str = "https://api.firecrawl.dev";
const XAI_BASE_URL_DEFAULT: &str = "https://api.x.ai/v1";
const XAI_WEB_MODEL_DEFAULT: &str = "grok-4.3";
const XAI_WEB_TIMEOUT_SECS_DEFAULT: u64 = 90;

fn secret_url_param(key: &str) -> bool {
    let key = key.trim().to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("authorization")
        || key.contains("credential")
        || (key.contains("api") && key.contains("key"))
        || key == "key"
}

fn validate_url_does_not_exfiltrate_secret(input: &str) -> Result<(), ToolError> {
    let Ok(parsed) = Url::parse(input) else {
        return Ok(());
    };
    for (key, value) in parsed.query_pairs() {
        if secret_url_param(&key) && !value.trim().is_empty() {
            return Err(ToolError::InvalidParams(format!(
                "Blocked URL: query parameter '{key}' looks like an API key or token; pass secrets via local env/vault, not web URLs."
            )));
        }
    }
    Ok(())
}

fn web_secret_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(api[_-]?key|token|secret|password|authorization|credential)\b\s*[:=]\s*["']?([A-Za-z0-9_\-./]{8,})"#,
        )
        .expect("web secret regex")
    })
}

fn redact_web_content(input: &str) -> String {
    web_secret_re()
        .replace_all(input, "$1=[REDACTED]")
        .to_string()
}

/// A web extraction backend that fetches HTML via reqwest with no external API dependency.
pub struct SimpleExtractBackend {
    client: Client,
}

/// Extract backend for search-only providers selected via legacy `web.backend`.
pub struct SearchOnlyExtractBackend {
    provider: &'static str,
}

impl SearchOnlyExtractBackend {
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl WebExtractBackend for SearchOnlyExtractBackend {
    async fn extract(&self, url: &str, _include_links: bool) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        Ok(json!({
            "success": false,
            "error": format!("{} is a search-only web backend and cannot extract URLs", self.provider),
            "url": url,
            "provider": self.provider,
        })
        .to_string())
    }
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
        validate_url_does_not_exfiltrate_secret(url)?;
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
                "content": redact_web_content(&text),
                "truncated": true,
                "original_size": bytes.len(),
            });
            return serde_json::to_string_pretty(&result)
                .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {}", e)));
        }

        let text = String::from_utf8_lossy(&bytes);

        let content = redact_web_content(&strip_html_tags(&text));

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
        let body = tavily_search_payload(&self.api_key, query, num_results, topic);

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

fn tavily_search_payload(api_key: &str, query: &str, num_results: usize, topic: &str) -> Value {
    json!({
        "api_key": api_key,
        "query": query,
        "max_results": num_results.min(20),
        "topic": topic,
        "search_depth": "basic",
        "include_answer": false,
        "include_images": false,
        "include_raw_content": false,
    })
}

/// Real Tavily API extract backend using `/extract`.
pub struct TavilyExtractBackend {
    client: Client,
    api_key: String,
    base_url: String,
}

impl TavilyExtractBackend {
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
impl WebExtractBackend for TavilyExtractBackend {
    async fn extract(&self, url: &str, _include_links: bool) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let body = json!({
            "api_key": self.api_key,
            "urls": [url],
            "include_images": false,
        });

        let resp = self
            .client
            .post(format!("{}/extract", self.base_url))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Tavily extract request failed: {}", e))
            })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Tavily extract response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Tavily extract API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Tavily extract response: {}", e))
        })?;
        let documents = normalize_tavily_documents(&data, url);
        let first_content = documents
            .first()
            .and_then(|d| d.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let result = json!({
            "url": url,
            "content": first_content,
            "results": documents,
            "provider": "tavily",
        });
        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize result: {}", e)))
    }
}

/// Real Tavily API crawl backend using `/crawl`.
pub struct TavilyCrawlBackend {
    client: Client,
    api_key: String,
    base_url: String,
}

impl TavilyCrawlBackend {
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
impl WebCrawlBackend for TavilyCrawlBackend {
    async fn crawl(
        &self,
        url: &str,
        instructions: Option<&str>,
        depth: &str,
        limit: usize,
    ) -> Result<String, ToolError> {
        let body = tavily_crawl_payload(&self.api_key, url, instructions, depth, limit);
        let resp = self
            .client
            .post(format!("{}/crawl", self.base_url))
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(60))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Tavily crawl request failed: {}", e))
            })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Tavily crawl response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Tavily crawl API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Tavily crawl response: {}", e))
        })?;
        let result = json!({
            "url": url,
            "results": normalize_tavily_documents(&data, url),
            "provider": "tavily",
        });
        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize result: {}", e)))
    }
}

fn tavily_crawl_payload(
    api_key: &str,
    url: &str,
    instructions: Option<&str>,
    depth: &str,
    limit: usize,
) -> Value {
    let mut payload = json!({
        "api_key": api_key,
        "url": url,
        "limit": limit,
        "extract_depth": if depth.trim().is_empty() { "basic" } else { depth.trim() },
    });
    if let Some(instructions) = instructions.map(str::trim).filter(|v| !v.is_empty()) {
        payload["instructions"] = json!(instructions);
    }
    payload
}

fn search_result(
    title: impl Into<String>,
    url: impl Into<String>,
    description: impl Into<String>,
    score: Option<f64>,
    position: usize,
) -> Value {
    let title = title.into();
    let url = url.into();
    let description = description.into();
    json!({
        "title": title,
        "url": url,
        "description": description,
        "text": description,
        "score": score.unwrap_or(0.0),
        "position": position,
    })
}

fn normalize_tavily_documents(response: &Value, fallback_url: &str) -> Vec<Value> {
    let mut documents = Vec::new();
    if let Some(results) = response.get("results").and_then(|v| v.as_array()) {
        for result in results {
            let url = result
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or(fallback_url);
            let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let raw = result
                .get("raw_content")
                .or_else(|| result.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            documents.push(json!({
                "url": url,
                "title": title,
                "content": raw,
                "raw_content": raw,
                "metadata": {"sourceURL": url, "title": title},
            }));
        }
    }
    if let Some(failed) = response.get("failed_results").and_then(|v| v.as_array()) {
        for failure in failed {
            let url = failure
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or(fallback_url);
            let error = failure
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("extraction failed");
            documents.push(json!({
                "url": url,
                "title": "",
                "content": "",
                "raw_content": "",
                "error": error,
                "metadata": {"sourceURL": url},
            }));
        }
    }
    if let Some(failed_urls) = response.get("failed_urls").and_then(|v| v.as_array()) {
        for failure in failed_urls {
            let url = failure
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| failure.to_string());
            documents.push(json!({
                "url": &url,
                "title": "",
                "content": "",
                "raw_content": "",
                "error": "extraction failed",
                "metadata": {"sourceURL": url},
            }));
        }
    }
    documents
}

// ---------------------------------------------------------------------------
// SearXNGSearchBackend
// ---------------------------------------------------------------------------

/// Real SearXNG backend using `/search?format=json`.
pub struct SearxngSearchBackend {
    client: Client,
    base_url: String,
}

impl SearxngSearchBackend {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim().trim_end_matches('/').to_string(),
        }
    }

    /// Create from `SEARXNG_BASE_URL` or upstream-compatible `SEARXNG_URL`.
    pub fn from_env() -> Result<Self, ToolError> {
        let base_url = std::env::var("SEARXNG_BASE_URL")
            .or_else(|_| std::env::var("SEARXNG_URL"))
            .map_err(|_| {
                ToolError::ExecutionFailed(
                    "SEARXNG_BASE_URL or SEARXNG_URL environment variable not set".into(),
                )
            })?;
        let base_url = base_url.trim();
        if base_url.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "SEARXNG_BASE_URL/SEARXNG_URL environment variable is empty".into(),
            ));
        }
        Ok(Self::new(base_url.to_string()))
    }

    fn endpoint(&self) -> String {
        format!("{}{}", self.base_url, SEARXNG_SEARCH_PATH)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait]
impl WebSearchBackend for SearxngSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut req = self.client.get(self.endpoint()).query(&[
            ("q", query),
            ("format", "json"),
            ("pageno", "1"),
        ]);
        if let Some(cat) = category.map(str::trim).filter(|v| !v.is_empty()) {
            req = req.query(&[("categories", cat)]);
        }

        let resp = req.send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("SearXNG API request failed: {}", e))
        })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read SearXNG response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "SearXNG API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse SearXNG response: {}", e))
        })?;
        let mut rows: Vec<Value> = data
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        let description = r
                            .get("content")
                            .or_else(|| r.get("snippet"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        search_result(
                            r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                            r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                            description,
                            r.get("score").and_then(|v| v.as_f64()),
                            0,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        rows.sort_by(|a, b| {
            let left = a.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let right = b.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            right
                .partial_cmp(&left)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (idx, row) in rows.iter_mut().take(num_results).enumerate() {
            row["position"] = json!(idx + 1);
        }
        let formatted: Vec<Value> = rows.into_iter().take(num_results).collect();

        serde_json::to_string_pretty(&json!({ "results": formatted }))
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// BraveFreeSearchBackend
// ---------------------------------------------------------------------------

/// Brave Search API backend using the free web endpoint.
pub struct BraveFreeSearchBackend {
    client: Client,
    api_key: String,
    endpoint: String,
}

impl BraveFreeSearchBackend {
    pub fn new(api_key: String, endpoint: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            endpoint,
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = std::env::var("BRAVE_SEARCH_API_KEY").map_err(|_| {
            ToolError::ExecutionFailed("BRAVE_SEARCH_API_KEY environment variable not set".into())
        })?;
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "BRAVE_SEARCH_API_KEY environment variable is empty".into(),
            ));
        }
        let endpoint = std::env::var("BRAVE_SEARCH_URL")
            .unwrap_or_else(|_| BRAVE_SEARCH_URL_DEFAULT.to_string());
        Ok(Self::new(api_key.to_string(), endpoint))
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl WebSearchBackend for BraveFreeSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        let count = num_results.clamp(1, 20).to_string();
        let resp = self
            .client
            .get(&self.endpoint)
            .header("X-Subscription-Token", &self.api_key)
            .query(&[("q", query), ("count", count.as_str())])
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Brave Search request failed: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Brave Search response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Brave Search API error ({status}): {text}"
            )));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Brave Search response: {e}"))
        })?;
        let formatted = normalize_brave_results(&data, num_results);
        serde_json::to_string_pretty(&json!({ "results": formatted, "provider": "brave-free" }))
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {e}")))
    }
}

fn normalize_brave_results(response: &Value, limit: usize) -> Vec<Value> {
    response
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(|results| results.as_array())
        .map(|rows| {
            rows.iter()
                .take(limit)
                .enumerate()
                .map(|(idx, row)| {
                    search_result(
                        row.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                        row.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                        row.get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        None,
                        idx + 1,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// DuckDuckGoSearchBackend
// ---------------------------------------------------------------------------

/// Keyless DuckDuckGo Instant Answer backend.
pub struct DuckDuckGoSearchBackend {
    client: Client,
    endpoint: String,
}

impl DuckDuckGoSearchBackend {
    pub fn new(endpoint: String) -> Self {
        Self {
            client: Client::new(),
            endpoint,
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        Ok(Self::new(std::env::var("DDG_SEARCH_URL").unwrap_or_else(
            |_| DDG_INSTANT_ANSWER_URL_DEFAULT.to_string(),
        )))
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl WebSearchBackend for DuckDuckGoSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        let resp = self
            .client
            .get(&self.endpoint)
            .query(&[
                ("q", query),
                ("format", "json"),
                ("no_html", "1"),
                ("skip_disambig", "1"),
            ])
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("DuckDuckGo search request failed: {e}"))
            })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read DuckDuckGo response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "DuckDuckGo API error ({status}): {text}"
            )));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse DuckDuckGo response: {e}"))
        })?;
        let formatted = normalize_duckduckgo_results(&data, num_results);
        serde_json::to_string_pretty(&json!({ "results": formatted, "provider": "ddgs" }))
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {e}")))
    }
}

fn normalize_duckduckgo_results(response: &Value, limit: usize) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Some(url) = response
        .get("AbstractURL")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
    {
        rows.push(search_result(
            response
                .get("Heading")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            url,
            response
                .get("AbstractText")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            None,
            0,
        ));
    }
    collect_duckduckgo_related(response.get("RelatedTopics"), &mut rows);
    for (idx, row) in rows.iter_mut().take(limit).enumerate() {
        row["position"] = json!(idx + 1);
    }
    rows.into_iter().take(limit).collect()
}

fn collect_duckduckgo_related(value: Option<&Value>, rows: &mut Vec<Value>) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for item in items {
        if let Some(topics) = item.get("Topics") {
            collect_duckduckgo_related(Some(topics), rows);
            continue;
        }
        let Some(url) = item
            .get("FirstURL")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
        else {
            continue;
        };
        let text = item.get("Text").and_then(|v| v.as_str()).unwrap_or("");
        let title = text.split(" - ").next().unwrap_or(text);
        rows.push(search_result(title, url, text, None, 0));
    }
}

// ---------------------------------------------------------------------------
// ParallelWebBackend
// ---------------------------------------------------------------------------

/// Parallel.ai search/extract backend.
///
/// With `PARALLEL_API_KEY`, this uses Parallel's v1 REST endpoints. Without a
/// key, it uses the hosted Search MCP endpoint with a neutral client identity.
pub struct ParallelWebBackend {
    client: Client,
    api_key: Option<String>,
    rest_base_url: String,
    mcp_url: String,
    search_mode: String,
}

impl ParallelWebBackend {
    pub fn from_env() -> Self {
        Self::with_endpoints(
            env_optional_nonempty("PARALLEL_API_KEY"),
            env_optional_nonempty("PARALLEL_BASE_URL")
                .unwrap_or_else(|| PARALLEL_BASE_URL_DEFAULT.to_string()),
            env_optional_nonempty("PARALLEL_MCP_URL")
                .unwrap_or_else(|| PARALLEL_MCP_SEARCH_URL_DEFAULT.to_string()),
            parallel_search_mode_from_env(),
        )
    }

    fn with_endpoints(
        api_key: Option<String>,
        rest_base_url: String,
        mcp_url: String,
        search_mode: String,
    ) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent(PARALLEL_MCP_USER_AGENT)
                .build()
                .unwrap_or_else(|_| Client::new()),
            api_key,
            rest_base_url: rest_base_url.trim().trim_end_matches('/').to_string(),
            mcp_url,
            search_mode,
        }
    }

    fn new_session_id() -> String {
        format!("{}-{}", PARALLEL_MCP_CLIENT_NAME, Uuid::new_v4().simple())
    }

    async fn search_rest(
        &self,
        api_key: &str,
        query: &str,
        limit: usize,
    ) -> Result<String, ToolError> {
        let body = json!({
            "search_queries": [query],
            "objective": query,
            "mode": self.search_mode,
            "session_id": Self::new_session_id(),
            "advanced_settings": {"max_results": limit.min(20)},
        });
        let data = self
            .post_json(
                &format!("{}/v1/search", self.rest_base_url),
                Some(api_key),
                &body,
            )
            .await?;
        let formatted = normalize_parallel_search_results(&data, limit);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "provider": "parallel",
        }))
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {e}")))
    }

    async fn search_mcp(&self, query: &str, limit: usize) -> Result<String, ToolError> {
        let payload = self
            .mcp_call(
                "web_search",
                json!({
                    "objective": query,
                    "search_queries": [query],
                    "session_id": Self::new_session_id(),
                }),
            )
            .await?;
        let formatted = normalize_parallel_search_results(&payload, limit);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "provider": "parallel",
            "attribution": PARALLEL_FREE_SEARCH_ATTRIBUTION,
        }))
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {e}")))
    }

    async fn extract_rest(&self, api_key: &str, url: &str) -> Result<String, ToolError> {
        let body = json!({
            "urls": [url],
            "advanced_settings": {"full_content": true},
            "session_id": Self::new_session_id(),
        });
        let data = self
            .post_json(
                &format!("{}/v1/extract", self.rest_base_url),
                Some(api_key),
                &body,
            )
            .await?;
        let documents = normalize_parallel_extract_documents(&data, &[url]);
        parallel_extract_response(url, documents, false)
    }

    async fn extract_mcp(&self, url: &str) -> Result<String, ToolError> {
        let payload = self
            .mcp_call(
                "web_fetch",
                json!({
                    "urls": [url],
                    "full_content": true,
                    "session_id": Self::new_session_id(),
                }),
            )
            .await?;
        let documents = normalize_parallel_extract_documents(&payload, &[url]);
        parallel_extract_response(url, documents, true)
    }

    async fn post_json(
        &self,
        endpoint: &str,
        api_key: Option<&str>,
        body: &Value,
    ) -> Result<Value, ToolError> {
        let mut req = self
            .client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, PARALLEL_MCP_USER_AGENT)
            .json(body);
        if let Some(api_key) = api_key.filter(|v| !v.trim().is_empty()) {
            req = req.bearer_auth(api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Parallel request failed: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Parallel response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Parallel API error ({status}): {text}"
            )));
        }
        serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Parallel response: {e}"))
        })
    }

    async fn mcp_call(&self, tool_name: &str, arguments: Value) -> Result<Value, ToolError> {
        let init_id = Uuid::new_v4().to_string();
        let init = self
            .mcp_post(
                None,
                None,
                &json!({
                    "jsonrpc": "2.0",
                    "id": init_id,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": PARALLEL_MCP_PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": {
                            "name": PARALLEL_MCP_CLIENT_NAME,
                            "version": PARALLEL_MCP_CLIENT_VERSION,
                        },
                    },
                }),
            )
            .await?;
        let mcp_session_id = init
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let init_text = init.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Failed to read Parallel MCP initialize response: {e}"
            ))
        })?;
        let init_envelope = parallel_mcp_response_envelope(&init_text, &init_id);
        let negotiated_version = init_envelope
            .get("result")
            .and_then(|v| v.get("protocolVersion"))
            .and_then(Value::as_str)
            .unwrap_or(PARALLEL_MCP_PROTOCOL_VERSION)
            .to_string();

        let _ = self
            .mcp_post(
                mcp_session_id.as_deref(),
                Some(&negotiated_version),
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            )
            .await?;

        let call_id = Uuid::new_v4().to_string();
        let call = self
            .mcp_post(
                mcp_session_id.as_deref(),
                Some(&negotiated_version),
                &json!({
                    "jsonrpc": "2.0",
                    "id": call_id,
                    "method": "tools/call",
                    "params": {"name": tool_name, "arguments": arguments},
                }),
            )
            .await?;
        let call_text = call.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Parallel MCP tool response: {e}"))
        })?;
        parallel_mcp_payload(&parallel_mcp_response_envelope(&call_text, &call_id))
    }

    async fn mcp_post(
        &self,
        mcp_session_id: Option<&str>,
        protocol_version: Option<&str>,
        body: &Value,
    ) -> Result<reqwest::Response, ToolError> {
        let mut req = self
            .client
            .post(&self.mcp_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::USER_AGENT, PARALLEL_MCP_USER_AGENT)
            .json(body);
        if let Some(session_id) = mcp_session_id.filter(|v| !v.trim().is_empty()) {
            req = req.header("Mcp-Session-Id", session_id);
        }
        if let Some(version) = protocol_version.filter(|v| !v.trim().is_empty()) {
            req = req.header("MCP-Protocol-Version", version);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Parallel MCP request failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "Parallel MCP error ({status}): {text}"
            )));
        }
        Ok(resp)
    }
}

#[async_trait]
impl WebSearchBackend for ParallelWebBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        let limit = num_results.clamp(1, 100);
        if let Some(api_key) = self.api_key.as_deref() {
            self.search_rest(api_key, query, limit).await
        } else {
            self.search_mcp(query, limit).await
        }
    }
}

#[async_trait]
impl WebExtractBackend for ParallelWebBackend {
    async fn extract(&self, url: &str, _include_links: bool) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        if let Some(api_key) = self.api_key.as_deref() {
            self.extract_rest(api_key, url).await
        } else {
            self.extract_mcp(url).await
        }
    }
}

fn parallel_search_mode_from_env() -> String {
    match env_optional_nonempty("PARALLEL_SEARCH_MODE")
        .unwrap_or_else(|| "advanced".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "basic" | "fast" | "one-shot" => "basic".to_string(),
        _ => "advanced".to_string(),
    }
}

fn normalize_parallel_search_results(response: &Value, limit: usize) -> Vec<Value> {
    let rows = response
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| response.pointer("/data/web").and_then(Value::as_array));
    rows.map(|rows| {
        rows.iter()
            .take(limit)
            .enumerate()
            .map(|(idx, row)| {
                let description = row
                    .get("excerpts")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| {
                        ["description", "snippet", "text", "content"]
                            .iter()
                            .find_map(|key| {
                                row.get(*key).and_then(Value::as_str).map(str::to_string)
                            })
                    })
                    .unwrap_or_default();
                search_result(
                    row.get("title").and_then(Value::as_str).unwrap_or(""),
                    row.get("url").and_then(Value::as_str).unwrap_or(""),
                    description,
                    row.get("score").and_then(Value::as_f64),
                    idx + 1,
                )
            })
            .collect()
    })
    .unwrap_or_default()
}

fn normalize_parallel_extract_documents(response: &Value, requested_urls: &[&str]) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Some(results) = response.get("results").and_then(Value::as_array) {
        for item in results {
            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
            let title = item.get("title").and_then(Value::as_str).unwrap_or("");
            let content = item
                .get("full_content")
                .or_else(|| item.get("raw_content"))
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    item.get("excerpts").and_then(Value::as_array).map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                })
                .unwrap_or_default();
            rows.push(json!({
                "url": url,
                "title": title,
                "content": content,
                "raw_content": content,
                "metadata": {"sourceURL": url, "title": title},
            }));
        }
    }
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        for item in errors {
            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
            let error = item
                .get("message")
                .or_else(|| item.get("content"))
                .or_else(|| item.get("error_type"))
                .or_else(|| item.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("extraction failed");
            rows.push(json!({
                "url": url,
                "title": "",
                "content": "",
                "error": error,
                "metadata": {"sourceURL": url},
            }));
        }
    }
    if rows.is_empty() {
        for url in requested_urls {
            rows.push(json!({
                "url": url,
                "title": "",
                "content": "",
                "error": "extraction failed (no content returned)",
                "metadata": {"sourceURL": url},
            }));
        }
    }
    rows
}

fn parallel_extract_response(
    url: &str,
    documents: Vec<Value>,
    free_mcp: bool,
) -> Result<String, ToolError> {
    let first_content = documents
        .first()
        .and_then(|d| d.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut response = json!({
        "url": url,
        "content": first_content,
        "results": documents,
        "provider": "parallel",
    });
    if free_mcp {
        response["attribution"] = json!(PARALLEL_FREE_EXTRACT_ATTRIBUTION);
    }
    serde_json::to_string_pretty(&response)
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize result: {e}")))
}

fn flatten_parallel_mcp_message(payload: Value, out: &mut Vec<Value>) {
    match payload {
        Value::Array(items) => out.extend(items),
        other => out.push(other),
    }
}

fn parallel_mcp_messages(text: &str) -> Vec<Value> {
    let body = text.trim();
    if body.is_empty() {
        return Vec::new();
    }
    if body.starts_with('{') || body.starts_with('[') {
        let Ok(payload) = serde_json::from_str::<Value>(body) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        flatten_parallel_mcp_message(payload, &mut out);
        return out;
    }

    let mut out = Vec::new();
    let mut data_lines: Vec<String> = Vec::new();
    for raw in body.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
            continue;
        }
        if line.trim().is_empty() && !data_lines.is_empty() {
            if let Ok(payload) = serde_json::from_str::<Value>(&data_lines.join("\n")) {
                flatten_parallel_mcp_message(payload, &mut out);
            }
            data_lines.clear();
        }
    }
    if !data_lines.is_empty() {
        if let Ok(payload) = serde_json::from_str::<Value>(&data_lines.join("\n")) {
            flatten_parallel_mcp_message(payload, &mut out);
        }
    }
    out
}

fn parallel_mcp_response_envelope(text: &str, request_id: &str) -> Value {
    let mut fallback = Value::Object(Default::default());
    for msg in parallel_mcp_messages(text) {
        let Some(obj) = msg.as_object() else {
            continue;
        };
        if !(obj.contains_key("result") || obj.contains_key("error")) {
            continue;
        }
        if msg.get("id").and_then(Value::as_str) == Some(request_id) {
            return msg;
        }
        fallback = msg;
    }
    fallback
}

fn parallel_mcp_payload(envelope: &Value) -> Result<Value, ToolError> {
    if let Some(error) = envelope.get("error") {
        return Err(ToolError::ExecutionFailed(format!(
            "Parallel MCP error: {}",
            truncate_json_for_error(error)
        )));
    }
    let result = envelope.get("result").cloned().unwrap_or_else(|| json!({}));
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ToolError::ExecutionFailed(format!(
            "Parallel MCP tool error: {}",
            truncate_json_for_error(&result)
        )));
    }
    if let Some(structured) = result.get("structuredContent").filter(|v| v.is_object()) {
        return Ok(structured.clone());
    }
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content {
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty());
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = text {
                    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                        return Ok(parsed);
                    }
                }
            }
        }
    }
    Err(ToolError::ExecutionFailed(format!(
        "Parallel MCP returned no parseable content: {}",
        truncate_json_for_error(&result)
    )))
}

fn truncate_json_for_error(value: &Value) -> String {
    let mut text = value.to_string();
    if text.len() > 500 {
        text.truncate(500);
    }
    text
}

/// Resolve preferred web-search backend from environment.
///
/// Priority:
/// 1. Explicit `HERMES_WEB_SEARCH_BACKEND` override, then legacy `HERMES_WEB_BACKEND`
///    - `exa`, `tavily`, `parallel`, `firecrawl`, `xai`, `searxng`, `brave-free`, `ddgs`, `fallback`
/// 2. Exa (`EXA_API_KEY`)
/// 3. Tavily (`TAVILY_API_KEY`, optional `TAVILY_BASE_URL`)
/// 4. Parallel REST (`PARALLEL_API_KEY`)
/// 5. Firecrawl (`FIRECRAWL_API_KEY`, `FIRECRAWL_API_URL`, or managed gateway)
/// 6. SearXNG (`SEARXNG_BASE_URL` or `SEARXNG_URL`)
/// 7. Brave (`BRAVE_SEARCH_API_KEY`)
/// 8. Keyless Parallel Search MCP fallback
pub fn search_backend_from_env_or_fallback() -> Box<dyn WebSearchBackend> {
    match search_backend_choice_from_env() {
        "exa" => ExaSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "tavily" => TavilySearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "parallel" => Box::new(ParallelWebBackend::from_env()),
        "firecrawl" => FirecrawlSearchBackend::from_env_or_managed()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "xai" => XaiWebSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "searxng" => SearxngSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "brave-free" => BraveFreeSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "ddgs" => DuckDuckGoSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        _ => Box::new(FallbackSearchBackend::new()),
    }
}

fn search_backend_choice_from_env() -> &'static str {
    if let Some(choice) = env_optional_nonempty("HERMES_WEB_SEARCH_BACKEND")
        .or_else(|| env_optional_nonempty("HERMES_WEB_BACKEND"))
    {
        if let Some(normalized) = normalize_search_backend_choice(&choice) {
            return normalized;
        }
    }

    if env_present_nonempty("EXA_API_KEY") {
        "exa"
    } else if env_present_nonempty("TAVILY_API_KEY") {
        "tavily"
    } else if env_present_nonempty("PARALLEL_API_KEY") {
        "parallel"
    } else if firecrawl_direct_config_present() || firecrawl_managed_config_present() {
        "firecrawl"
    } else if searxng_config_present() {
        "searxng"
    } else if env_present_nonempty("BRAVE_SEARCH_API_KEY") {
        "brave-free"
    } else {
        "parallel"
    }
}

fn normalize_search_backend_choice(choice: &str) -> Option<&'static str> {
    match choice.trim().to_ascii_lowercase().as_str() {
        "exa" => Some("exa"),
        "tavily" => Some("tavily"),
        "parallel" => Some("parallel"),
        "firecrawl" => Some("firecrawl"),
        "xai" | "grok" => Some("xai"),
        "searxng" | "searx" => Some("searxng"),
        "brave" | "brave-free" | "brave_free" => Some("brave-free"),
        "ddg" | "ddgs" | "duckduckgo" => Some("ddgs"),
        "fallback" | "none" | "off" | "disabled" => Some("fallback"),
        _ => None,
    }
}

/// Resolve preferred web-extract backend from environment.
///
/// Priority:
/// 1. Explicit `HERMES_WEB_EXTRACT_BACKEND` override, then legacy `HERMES_WEB_BACKEND`
///    (`parallel`, `firecrawl`, `tavily`, `simple`; search-only backends return a clear error)
/// 2. Firecrawl direct/self-hosted/managed when configured
/// 3. Tavily when configured
/// 4. Parallel REST/MCP fallback
pub fn extract_backend_from_env_or_fallback() -> Box<dyn WebExtractBackend> {
    match extract_backend_choice_from_env() {
        "parallel" => Box::new(ParallelWebBackend::from_env()),
        "firecrawl" => FirecrawlExtractBackend::from_env_or_managed()
            .map(|b| Box::new(b) as Box<dyn WebExtractBackend>)
            .unwrap_or_else(|_| Box::new(SimpleExtractBackend::new())),
        "tavily" => TavilyExtractBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebExtractBackend>)
            .unwrap_or_else(|_| Box::new(SimpleExtractBackend::new())),
        "search-only:brave-free" => Box::new(SearchOnlyExtractBackend::new("brave-free")),
        "search-only:ddgs" => Box::new(SearchOnlyExtractBackend::new("ddgs")),
        "search-only:searxng" => Box::new(SearchOnlyExtractBackend::new("searxng")),
        "search-only:exa" => Box::new(SearchOnlyExtractBackend::new("exa")),
        "search-only:xai" => Box::new(SearchOnlyExtractBackend::new("xai")),
        _ => Box::new(SimpleExtractBackend::new()),
    }
}

fn extract_backend_choice_from_env() -> &'static str {
    if let Ok(choice) = std::env::var("HERMES_WEB_EXTRACT_BACKEND") {
        match choice.trim().to_ascii_lowercase().as_str() {
            "parallel" => return "parallel",
            "firecrawl" => return "firecrawl",
            "tavily" => return "tavily",
            "simple" | "fallback" | "local" | "none" | "off" | "disabled" => return "simple",
            _ => {}
        }
    }
    if let Some(choice) = env_optional_nonempty("HERMES_WEB_BACKEND")
        .and_then(|choice| normalize_search_backend_choice(&choice).map(str::to_string))
    {
        match choice.as_str() {
            "parallel" => return "parallel",
            "firecrawl" => return "firecrawl",
            "tavily" => return "tavily",
            "brave-free" => return "search-only:brave-free",
            "ddgs" => return "search-only:ddgs",
            "searxng" => return "search-only:searxng",
            "exa" => return "search-only:exa",
            "xai" => return "search-only:xai",
            _ => {}
        }
    }

    if firecrawl_direct_config_present() || firecrawl_managed_config_present() {
        "firecrawl"
    } else if env_present_nonempty("TAVILY_API_KEY") {
        "tavily"
    } else {
        "parallel"
    }
}

/// Resolve preferred web-crawl backend from environment.
///
/// Priority:
/// 1. Explicit `HERMES_WEB_CRAWL_BACKEND` override: `tavily`, `fallback`
/// 2. Tavily when configured
/// 3. Fallback helpful message backend
pub fn crawl_backend_from_env_or_fallback() -> Box<dyn WebCrawlBackend> {
    match crawl_backend_choice_from_env() {
        "tavily" => TavilyCrawlBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebCrawlBackend>)
            .unwrap_or_else(|_| Box::new(FallbackCrawlBackend::new())),
        _ => Box::new(FallbackCrawlBackend::new()),
    }
}

fn crawl_backend_choice_from_env() -> &'static str {
    if let Ok(choice) = std::env::var("HERMES_WEB_CRAWL_BACKEND") {
        match choice.trim().to_ascii_lowercase().as_str() {
            "tavily" => return "tavily",
            "fallback" | "none" | "off" | "disabled" => return "fallback",
            _ => {}
        }
    }
    if let Some(choice) = env_optional_nonempty("HERMES_WEB_BACKEND")
        .and_then(|choice| normalize_search_backend_choice(&choice).map(str::to_string))
    {
        if choice == "tavily" {
            return "tavily";
        }
    }

    if env_present_nonempty("TAVILY_API_KEY") {
        "tavily"
    } else {
        "fallback"
    }
}

fn env_present_nonempty(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn searxng_config_present() -> bool {
    env_present_nonempty("SEARXNG_BASE_URL") || env_present_nonempty("SEARXNG_URL")
}

fn env_optional_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// XaiWebSearchBackend
// ---------------------------------------------------------------------------

/// xAI Responses API search backend using the server-side `web_search` tool.
pub struct XaiWebSearchBackend {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
    allowed_domains: Vec<String>,
    excluded_domains: Vec<String>,
    timeout_secs: u64,
}

impl XaiWebSearchBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: XAI_BASE_URL_DEFAULT.to_string(),
            model: XAI_WEB_MODEL_DEFAULT.to_string(),
            allowed_domains: Vec::new(),
            excluded_domains: Vec::new(),
            timeout_secs: XAI_WEB_TIMEOUT_SECS_DEFAULT,
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = env_optional_nonempty("XAI_API_KEY").ok_or_else(|| {
            ToolError::ExecutionFailed(
                "XAI_API_KEY environment variable not set for xAI web search".into(),
            )
        })?;
        let mut backend = Self::new(api_key);
        if let Some(base_url) = env_optional_nonempty("XAI_BASE_URL") {
            backend.base_url = base_url.trim_end_matches('/').to_string();
        }
        if let Some(model) = env_optional_nonempty("HERMES_WEB_XAI_MODEL") {
            backend.model = model;
        }
        backend.allowed_domains = parse_domain_filter_env("HERMES_WEB_XAI_ALLOWED_DOMAINS");
        backend.excluded_domains = parse_domain_filter_env("HERMES_WEB_XAI_EXCLUDED_DOMAINS");
        if !backend.allowed_domains.is_empty() && !backend.excluded_domains.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "HERMES_WEB_XAI_ALLOWED_DOMAINS and HERMES_WEB_XAI_EXCLUDED_DOMAINS cannot both be set"
                    .into(),
            ));
        }
        if let Some(timeout) = env_optional_nonempty("HERMES_WEB_XAI_TIMEOUT") {
            backend.timeout_secs = timeout
                .parse::<u64>()
                .unwrap_or(XAI_WEB_TIMEOUT_SECS_DEFAULT);
        }
        Ok(backend)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn build_prompt(query: &str, limit: usize) -> String {
        format!(
            "Use the web_search tool to find current information for the query below, \
then respond with ONLY a single JSON object - no prose, no markdown fences, \
no inline citation links - matching this exact schema:\n\n\
{{\"results\": [{{\"title\": \"string\", \"url\": \"string\", \"description\": \"1-2 sentence summary\"}}]}}\n\n\
Return at most {limit} results, ordered by relevance, with absolute https:// URLs. \
If no usable results exist, return {{\"results\": []}}.\n\n\
Query: {query}"
        )
    }

    fn parse_results(response_data: &Value, limit: usize) -> Vec<Value> {
        let (text_blocks, annotations) = collect_xai_output_text(response_data);
        for block in &text_blocks {
            let parsed = parse_xai_json_results(block, limit);
            if !parsed.is_empty() {
                return parsed;
            }
        }

        if !annotations.is_empty() {
            let joined = text_blocks.join("\n");
            let parsed = xai_results_from_annotations(&annotations, &joined, limit);
            if !parsed.is_empty() {
                return parsed;
            }
        }

        response_data
            .get("citations")
            .and_then(|v| v.as_array())
            .map(|citations| {
                citations
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(|url| !url.trim().is_empty())
                    .take(limit)
                    .enumerate()
                    .map(|(idx, url)| {
                        json!({
                            "title": "",
                            "url": url,
                            "description": "",
                            "position": idx + 1,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl WebSearchBackend for XaiWebSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        if !self.excluded_domains.is_empty() && !self.allowed_domains.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "xAI allowed_domains and excluded_domains cannot both be set".into(),
            ));
        }

        let limit = num_results.clamp(1, 100);
        let mut web_search_tool = json!({"type": "web_search"});
        if !self.allowed_domains.is_empty() {
            web_search_tool["filters"] = json!({"allowed_domains": &self.allowed_domains});
        } else if !self.excluded_domains.is_empty() {
            web_search_tool["filters"] = json!({"excluded_domains": &self.excluded_domains});
        }

        let payload = json!({
            "model": &self.model,
            "input": [{"role": "user", "content": Self::build_prompt(query, limit)}],
            "tools": [web_search_tool],
            "include": ["no_inline_citations"],
        });

        let resp = self
            .client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .header("User-Agent", "hermes-agent-ultra/rust-web-search")
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("xAI web search request failed: {e}"))
            })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read xAI web search response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "xAI web search API error ({status}): {text}"
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse xAI web search response: {e}"))
        })?;
        if let Some(api_error) = data.get("error").and_then(|v| v.as_object()) {
            let message = api_error
                .get("message")
                .or_else(|| api_error.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(ToolError::ExecutionFailed(format!(
                "xAI returned an error: {message}"
            )));
        }

        serde_json::to_string_pretty(&json!({
            "results": Self::parse_results(&data, limit),
            "provider": "xai",
            "model": &self.model,
        }))
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {e}")))
    }
}

fn parse_domain_filter_env(name: &str) -> Vec<String> {
    env_optional_nonempty(name)
        .map(|value| {
            value
                .split(',')
                .filter_map(|part| {
                    let domain = part.trim();
                    if domain.is_empty() {
                        None
                    } else {
                        Some(domain.to_string())
                    }
                })
                .take(5)
                .collect()
        })
        .unwrap_or_default()
}

fn collect_xai_output_text(response_data: &Value) -> (Vec<String>, Vec<Value>) {
    let mut text_blocks = Vec::new();
    let mut annotations = Vec::new();
    if let Some(output) = response_data.get("output").and_then(|v| v.as_array()) {
        for item in output {
            if item.get("type").and_then(|v| v.as_str()) != Some("message") {
                continue;
            }
            let Some(content) = item.get("content").and_then(|v| v.as_array()) else {
                continue;
            };
            for chunk in content {
                if chunk.get("type").and_then(|v| v.as_str()) != Some("output_text") {
                    continue;
                }
                if let Some(text) = chunk.get("text").and_then(|v| v.as_str()) {
                    if !text.trim().is_empty() {
                        text_blocks.push(text.to_string());
                    }
                }
                if let Some(chunk_annotations) = chunk.get("annotations").and_then(|v| v.as_array())
                {
                    annotations.extend(chunk_annotations.iter().cloned());
                }
            }
        }
    }
    (text_blocks, annotations)
}

fn parse_xai_json_results(text: &str, limit: usize) -> Vec<Value> {
    let mut candidates = vec![text];
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            candidates.push(&text[start..=end]);
        }
    }

    for candidate in candidates {
        let Ok(parsed) = serde_json::from_str::<Value>(candidate) else {
            continue;
        };
        let Some(results) = parsed.get("results").and_then(|v| v.as_array()) else {
            continue;
        };
        let mut normalized = Vec::new();
        for row in results.iter().take(limit) {
            let Some(url) = row.get("url").and_then(|v| v.as_str()).map(str::trim) else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            normalized.push(json!({
                "title": row.get("title").and_then(|v| v.as_str()).unwrap_or("").trim(),
                "url": url,
                "description": row.get("description").and_then(|v| v.as_str()).unwrap_or("").trim(),
                "position": normalized.len() + 1,
            }));
        }
        if !normalized.is_empty() {
            return normalized;
        }
    }
    Vec::new()
}

fn xai_results_from_annotations(
    annotations: &[Value],
    joined_text: &str,
    limit: usize,
) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut results = Vec::new();
    for annotation in annotations {
        if annotation.get("type").and_then(|v| v.as_str()) != Some("url_citation") {
            continue;
        }
        let Some(url) = annotation
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
        else {
            continue;
        };
        if url.is_empty() || !seen.insert(url.to_string()) {
            continue;
        }
        let description = annotation
            .get("start_index")
            .and_then(|v| v.as_u64())
            .and_then(|idx| {
                let idx = idx as usize;
                if joined_text.is_char_boundary(idx) {
                    Some(
                        joined_text[..idx]
                            .chars()
                            .rev()
                            .take(200)
                            .collect::<String>(),
                    )
                } else {
                    None
                }
            })
            .map(|s| s.chars().rev().collect::<String>().trim().to_string())
            .unwrap_or_default();
        results.push(json!({
            "title": "",
            "url": url,
            "description": description,
            "position": results.len() + 1,
        }));
        if results.len() >= limit {
            break;
        }
    }
    results
}

// ---------------------------------------------------------------------------
// Firecrawl backends
// ---------------------------------------------------------------------------

/// Identifies how a Firecrawl request reaches the API. Reflected in the
/// returned JSON's `transport` field for observability.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FirecrawlTransport {
    /// Direct call to Firecrawl Cloud or a self-hosted Firecrawl endpoint.
    Direct {
        endpoint_root: String,
        api_key: Option<String>,
    },
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

    fn endpoint(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        match self {
            Self::Direct { endpoint_root, .. } | Self::Managed { endpoint_root, .. } => {
                format!("{endpoint_root}/v1/{path}")
            }
        }
    }

    fn auth_bearer(&self) -> Option<&str> {
        match self {
            Self::Direct { api_key, .. } => api_key.as_deref(),
            Self::Managed { nous_token, .. } => Some(nous_token),
        }
    }
}

fn normalize_firecrawl_endpoint_root(raw: &str) -> String {
    let mut root = raw.trim().trim_end_matches('/').to_string();
    if root.ends_with("/v1") {
        root.truncate(root.len() - 3);
    }
    if root.is_empty() {
        FIRECRAWL_BASE_URL_DEFAULT.to_string()
    } else {
        root
    }
}

fn firecrawl_direct_config_present() -> bool {
    env_present_nonempty("FIRECRAWL_API_KEY") || env_present_nonempty("FIRECRAWL_API_URL")
}

fn firecrawl_managed_config_present() -> bool {
    is_managed_tool_gateway_ready("firecrawl", ResolveOptions::default())
}

fn firecrawl_transport_from_env_or_managed() -> Result<FirecrawlTransport, ToolError> {
    let api_key = env_optional_nonempty("FIRECRAWL_API_KEY");
    let api_url = env_optional_nonempty("FIRECRAWL_API_URL");
    if api_key.is_some() || api_url.is_some() {
        let endpoint_root = api_url
            .as_deref()
            .map(normalize_firecrawl_endpoint_root)
            .unwrap_or_else(|| FIRECRAWL_BASE_URL_DEFAULT.to_string());
        return Ok(FirecrawlTransport::Direct {
            endpoint_root,
            api_key,
        });
    }
    if let Some(cfg) = resolve_managed_tool_gateway("firecrawl", ResolveOptions::default()) {
        return Ok(FirecrawlTransport::Managed {
            endpoint_root: normalize_firecrawl_endpoint_root(&cfg.gateway_origin),
            nous_token: cfg.nous_user_token,
        });
    }
    Err(ToolError::ExecutionFailed(
        "FIRECRAWL_API_KEY/FIRECRAWL_API_URL not set and Nous-managed firecrawl gateway is not configured."
            .into(),
    ))
}

/// Firecrawl search backend using `/v1/search`.
#[derive(Debug)]
pub struct FirecrawlSearchBackend {
    client: Client,
    transport: FirecrawlTransport,
}

impl FirecrawlSearchBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            transport: FirecrawlTransport::Direct {
                endpoint_root: FIRECRAWL_BASE_URL_DEFAULT.to_string(),
                api_key: Some(api_key),
            },
        }
    }

    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            client: Client::new(),
            transport: FirecrawlTransport::Managed {
                endpoint_root: normalize_firecrawl_endpoint_root(&cfg.gateway_origin),
                nous_token: cfg.nous_user_token.clone(),
            },
        }
    }

    pub fn from_env_or_managed() -> Result<Self, ToolError> {
        Ok(Self {
            client: Client::new(),
            transport: firecrawl_transport_from_env_or_managed()?,
        })
    }

    pub fn transport_label(&self) -> &'static str {
        self.transport.label()
    }
}

#[async_trait]
impl WebSearchBackend for FirecrawlSearchBackend {
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        let body = json!({
            "query": query,
            "limit": num_results,
        });
        let mut req = self
            .client
            .post(self.transport.endpoint("search"))
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(token) = self.transport.auth_bearer() {
            req = req.header("Authorization", format!("Bearer {token}"));
        }

        let resp = req.send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Firecrawl search request failed: {}", e))
        })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Firecrawl search response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Firecrawl search API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Firecrawl search response: {}", e))
        })?;
        let formatted = normalize_firecrawl_search_results(&data);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "transport": self.transport.label(),
        }))
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {}", e)))
    }
}

fn normalize_firecrawl_search_results(response: &Value) -> Vec<Value> {
    let candidates = [
        response.get("data"),
        response.get("data").and_then(|d| d.get("web")),
        response.get("data").and_then(|d| d.get("results")),
        response.get("web"),
        response.get("results"),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|v| v.as_array())
        .map(|results| {
            results
                .iter()
                .map(|result| {
                    let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let text = result
                        .get("description")
                        .or_else(|| result.get("content"))
                        .or_else(|| result.get("markdown"))
                        .or_else(|| result.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    json!({
                        "title": title,
                        "url": url,
                        "text": text,
                        "score": result.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
            transport: FirecrawlTransport::Direct {
                endpoint_root: FIRECRAWL_BASE_URL_DEFAULT.to_string(),
                api_key: Some(api_key),
            },
        }
    }

    /// Construct a managed-mode backend from a resolved gateway config.
    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            client: Client::new(),
            transport: FirecrawlTransport::Managed {
                endpoint_root: normalize_firecrawl_endpoint_root(&cfg.gateway_origin),
                nous_token: cfg.nous_user_token.clone(),
            },
        }
    }

    /// Resolve the best-available transport.
    ///
    /// Priority: direct `FIRECRAWL_API_KEY` / `FIRECRAWL_API_URL` →
    /// Nous-managed `firecrawl` vendor → `Err` with a hint covering both paths.
    pub fn from_env_or_managed() -> Result<Self, ToolError> {
        Ok(Self {
            client: Client::new(),
            transport: firecrawl_transport_from_env_or_managed()?,
        })
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
        validate_url_does_not_exfiltrate_secret(url)?;
        let body = json!({
            "url": url,
            "formats": ["markdown"],
            "onlyMainContent": true,
            "includeLinks": include_links,
        });

        let resp = self
            .client
            .post(self.transport.endpoint("scrape"))
            .header("Content-Type", "application/json")
            .json(&body);
        let resp = if let Some(token) = self.transport.auth_bearer() {
            resp.header("Authorization", format!("Bearer {token}"))
        } else {
            resp
        }
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Firecrawl API request failed: {}", e)))?;

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
            let keys = [
                "EXA_API_KEY",
                "TAVILY_API_KEY",
                "TAVILY_BASE_URL",
                "FIRECRAWL_API_KEY",
                "FIRECRAWL_API_URL",
                "PARALLEL_API_KEY",
                "PARALLEL_BASE_URL",
                "PARALLEL_MCP_URL",
                "PARALLEL_SEARCH_MODE",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
                "SEARXNG_BASE_URL",
                "SEARXNG_URL",
                "BRAVE_SEARCH_API_KEY",
                "BRAVE_SEARCH_URL",
                "DDG_SEARCH_URL",
                "XAI_API_KEY",
                "XAI_BASE_URL",
                "HERMES_WEB_BACKEND",
                "HERMES_WEB_SEARCH_BACKEND",
                "HERMES_WEB_EXTRACT_BACKEND",
                "HERMES_WEB_CRAWL_BACKEND",
                "HERMES_WEB_XAI_MODEL",
                "HERMES_WEB_XAI_ALLOWED_DOMAINS",
                "HERMES_WEB_XAI_EXCLUDED_DOMAINS",
                "HERMES_WEB_XAI_TIMEOUT",
            ];
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
    fn web_extract_url_guard_blocks_secret_query_params() {
        let err = validate_url_does_not_exfiltrate_secret(
            "https://example.com/page?access_token=secret-token-123456789",
        )
        .expect_err("access token should be blocked");
        assert!(err.to_string().contains("access_token"));

        validate_url_does_not_exfiltrate_secret("https://example.com/page?q=token rotation")
            .expect("ordinary search query should be allowed");
    }

    #[test]
    fn web_content_redaction_removes_secret_values() {
        let redacted =
            redact_web_content("Dashboard password = hunter2token token: abcdefghijklmnop");
        assert!(!redacted.contains("hunter2token"));
        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(redacted.contains("[REDACTED]"));
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
    fn tavily_search_payload_caps_max_results_at_provider_limit() {
        assert_eq!(
            tavily_search_payload("key", "rust", 50, "general"),
            json!({
                "api_key": "key",
                "query": "rust",
                "max_results": 20,
                "topic": "general",
                "search_depth": "basic",
                "include_answer": false,
                "include_images": false,
                "include_raw_content": false,
            })
        );
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

    #[test]
    fn searxng_from_env_normalizes_base_url() {
        let _scope = EnvScope::new();
        std::env::set_var("SEARXNG_BASE_URL", "https://search.example.com/");
        let backend = SearxngSearchBackend::from_env().expect("searxng backend from env");
        assert_eq!(backend.base_url(), "https://search.example.com");
    }

    #[test]
    fn searxng_from_env_accepts_upstream_url_alias() {
        let _scope = EnvScope::new();
        std::env::set_var("SEARXNG_URL", "https://search.example.com/");
        let backend = SearxngSearchBackend::from_env().expect("searxng backend from alias");
        assert_eq!(backend.base_url(), "https://search.example.com");
    }

    #[test]
    fn search_backend_choice_uses_searxng_when_only_base_url_available() {
        let _scope = EnvScope::new();
        std::env::set_var("SEARXNG_BASE_URL", "https://search.example.com");
        assert_eq!(search_backend_choice_from_env(), "searxng");
    }

    #[test]
    fn search_backend_choice_uses_firecrawl_when_configured() {
        let _scope = EnvScope::new();
        std::env::set_var("FIRECRAWL_API_URL", "http://127.0.0.1:3002/v1");
        assert_eq!(search_backend_choice_from_env(), "firecrawl");
    }

    #[test]
    fn search_backend_choice_uses_xai_only_when_explicit() {
        let _scope = EnvScope::new();
        std::env::set_var("XAI_API_KEY", "xai-key");
        assert_eq!(search_backend_choice_from_env(), "parallel");
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "xai");
        assert_eq!(search_backend_choice_from_env(), "xai");
    }

    #[test]
    fn search_backend_choice_honors_explicit_override() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "searxng");
        std::env::set_var("EXA_API_KEY", "exa-key");
        assert_eq!(search_backend_choice_from_env(), "searxng");
    }

    #[test]
    fn search_backend_choice_honors_legacy_generic_web_backend() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_BACKEND", "brave");
        std::env::set_var("EXA_API_KEY", "exa-key");
        assert_eq!(search_backend_choice_from_env(), "brave-free");
    }

    #[test]
    fn search_backend_choice_prefers_per_capability_override_over_generic_backend() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_BACKEND", "firecrawl");
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "tavily");
        std::env::set_var("FIRECRAWL_API_KEY", "fire-key");
        std::env::set_var("TAVILY_API_KEY", "tavily-key");

        assert_eq!(search_backend_choice_from_env(), "tavily");
    }

    #[test]
    fn search_backend_choice_uses_brave_when_key_is_available() {
        let _scope = EnvScope::new();
        std::env::set_var("BRAVE_SEARCH_API_KEY", "brave-key");
        assert_eq!(search_backend_choice_from_env(), "brave-free");
    }

    #[test]
    fn search_backend_choice_uses_keyless_parallel_as_last_resort() {
        let _scope = EnvScope::new();
        assert_eq!(search_backend_choice_from_env(), "parallel");
    }

    #[test]
    fn search_backend_choice_accepts_explicit_ddg() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "ddgs");
        assert_eq!(search_backend_choice_from_env(), "ddgs");
    }

    #[test]
    fn search_backend_choice_uses_parallel_key_when_present() {
        let _scope = EnvScope::new();
        std::env::set_var("PARALLEL_API_KEY", "parallel-key");
        assert_eq!(search_backend_choice_from_env(), "parallel");
    }

    #[tokio::test]
    async fn search_backend_falls_back_when_explicitly_disabled() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "fallback");
        let backend = search_backend_from_env_or_fallback();
        let out = backend
            .search("hello", 3, None)
            .await
            .expect("fallback backend should return json");
        assert!(out.contains("\"no_api_key\""));
    }

    #[test]
    fn extract_backend_choice_prefers_firecrawl_then_tavily_then_simple() {
        let _scope = EnvScope::new();
        assert_eq!(extract_backend_choice_from_env(), "parallel");
        std::env::set_var("TAVILY_API_KEY", "tavily-key");
        assert_eq!(extract_backend_choice_from_env(), "tavily");
        std::env::set_var("FIRECRAWL_API_KEY", "fire-key");
        assert_eq!(extract_backend_choice_from_env(), "firecrawl");
    }

    #[test]
    fn extract_backend_choice_accepts_explicit_simple_and_parallel() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_EXTRACT_BACKEND", "simple");
        assert_eq!(extract_backend_choice_from_env(), "simple");
        std::env::set_var("HERMES_WEB_EXTRACT_BACKEND", "parallel");
        assert_eq!(extract_backend_choice_from_env(), "parallel");
    }

    #[test]
    fn extract_backend_choice_reports_search_only_generic_backend() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_BACKEND", "ddgs");
        assert_eq!(extract_backend_choice_from_env(), "search-only:ddgs");
    }

    #[test]
    fn extract_backend_choice_prefers_per_capability_override_over_generic_backend() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_BACKEND", "tavily");
        std::env::set_var("HERMES_WEB_EXTRACT_BACKEND", "simple");
        std::env::set_var("TAVILY_API_KEY", "tavily-key");

        assert_eq!(extract_backend_choice_from_env(), "simple");
    }

    #[test]
    fn crawl_backend_choice_uses_tavily_when_configured() {
        let _scope = EnvScope::new();
        assert_eq!(crawl_backend_choice_from_env(), "fallback");
        std::env::set_var("TAVILY_API_KEY", "tavily-key");
        assert_eq!(crawl_backend_choice_from_env(), "tavily");
        std::env::set_var("HERMES_WEB_CRAWL_BACKEND", "fallback");
        assert_eq!(crawl_backend_choice_from_env(), "fallback");
    }

    #[test]
    fn tavily_crawl_payload_includes_body_auth_and_options() {
        assert_eq!(
            tavily_crawl_payload(
                "key",
                "https://seed.example",
                Some(" docs only "),
                "advanced",
                12
            ),
            json!({
                "api_key": "key",
                "url": "https://seed.example",
                "limit": 12,
                "extract_depth": "advanced",
                "instructions": "docs only",
            })
        );
    }

    #[test]
    fn normalize_tavily_documents_maps_results_and_failures() {
        let docs = normalize_tavily_documents(
            &json!({
                "results": [{"url": "https://ok.example", "title": "OK", "raw_content": "body"}],
                "failed_results": [{"url": "https://bad.example", "error": "blocked"}],
                "failed_urls": ["https://missing.example", 42]
            }),
            "https://fallback.example",
        );
        assert_eq!(docs.len(), 4);
        assert_eq!(docs[0]["content"], "body");
        assert_eq!(docs[1]["error"], "blocked");
        assert_eq!(docs[2]["url"], "https://missing.example");
        assert_eq!(docs[3]["url"], "42");
    }

    #[test]
    fn normalize_firecrawl_search_results_accepts_nested_web_shape() {
        let rows = normalize_firecrawl_search_results(&json!({
            "data": {
                "web": [{"title": "Rust", "url": "https://rust-lang.org", "description": "lang"}]
            }
        }));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], "Rust");
        assert_eq!(rows[0]["text"], "lang");
    }

    #[test]
    fn normalize_brave_results_maps_positions_and_limit() {
        let rows = normalize_brave_results(
            &json!({
                "web": {
                    "results": [
                        {"title": "A", "url": "https://a.example", "description": "desc a"},
                        {"title": "B", "url": "https://b.example", "description": "desc b"}
                    ]
                }
            }),
            1,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], "A");
        assert_eq!(rows[0]["description"], "desc a");
        assert_eq!(rows[0]["position"], 1);
    }

    #[test]
    fn normalize_duckduckgo_results_flattens_related_topics() {
        let rows = normalize_duckduckgo_results(
            &json!({
                "RelatedTopics": [
                    {"Text": "A - desc a", "FirstURL": "https://a.example"},
                    {"Topics": [
                        {"Text": "B - desc b", "FirstURL": "https://b.example"}
                    ]}
                ]
            }),
            5,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["title"], "A");
        assert_eq!(rows[1]["url"], "https://b.example");
        assert_eq!(rows[1]["position"], 2);
    }

    #[test]
    fn parallel_search_mode_maps_legacy_values() {
        let _scope = EnvScope::new();
        std::env::set_var("PARALLEL_SEARCH_MODE", "fast");
        assert_eq!(parallel_search_mode_from_env(), "basic");
        std::env::set_var("PARALLEL_SEARCH_MODE", "agentic");
        assert_eq!(parallel_search_mode_from_env(), "advanced");
    }

    #[test]
    fn parallel_mcp_messages_parse_plain_json_and_sse() {
        let plain = parallel_mcp_messages(
            r#"[{"jsonrpc":"2.0","id":"a","result":{"ok":1}},{"method":"n"}]"#,
        );
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[0]["id"], "a");

        let sse = parallel_mcp_response_envelope(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":\"noise\",\"result\":{\"skip\":true}}\n\n\
             data: {\"jsonrpc\":\"2.0\",\"id\":\"target\",\"result\":{\"structuredContent\":{\"results\":[{\"url\":\"https://a.example\"}]}}}\n\n",
            "target",
        );
        let payload = parallel_mcp_payload(&sse).expect("mcp payload");
        assert_eq!(payload["results"][0]["url"], "https://a.example");
    }

    #[test]
    fn parallel_normalizers_map_search_and_extract_shapes() {
        let rows = normalize_parallel_search_results(
            &json!({
                "results": [
                    {"url": "https://a.example", "title": "A", "excerpts": ["one", "two"]},
                    {"url": "https://b.example", "title": "B", "description": "desc"}
                ]
            }),
            1,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["description"], "one two");
        assert_eq!(rows[0]["position"], 1);

        let docs = normalize_parallel_extract_documents(
            &json!({
                "results": [{"url": "https://a.example", "title": "A", "full_content": "body"}],
                "errors": [{"url": "https://b.example", "message": "blocked"}]
            }),
            &["https://a.example"],
        );
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0]["content"], "body");
        assert_eq!(docs[1]["error"], "blocked");
    }

    #[tokio::test]
    async fn parallel_keyless_mcp_search_uses_generic_client_identity() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("user-agent", PARALLEL_MCP_USER_AGENT))
            .and(body_partial_json(json!({
                "method": "initialize",
                "params": {"clientInfo": {"name": PARALLEL_MCP_CLIENT_NAME}},
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("mcp-session-id", "sess-1")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": "ignored-by-fallback",
                        "result": {"protocolVersion": PARALLEL_MCP_PROTOCOL_VERSION},
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("mcp-session-id", "sess-1"))
            .and(header(
                "mcp-protocol-version",
                PARALLEL_MCP_PROTOCOL_VERSION,
            ))
            .and(body_partial_json(
                json!({"method": "notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202).set_body_string(""))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("mcp-session-id", "sess-1"))
            .and(body_partial_json(json!({
                "method": "tools/call",
                "params": {
                    "name": "web_search",
                    "arguments": {"objective": "rust async"},
                },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: message\n\
                 data: {\"jsonrpc\":\"2.0\",\"id\":\"fallback\",\"result\":{\"structuredContent\":{\"results\":[{\"url\":\"https://rust-lang.org\",\"title\":\"Rust\",\"excerpts\":[\"Language\"]}]}}}\n\n",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ParallelWebBackend::with_endpoints(
            None,
            PARALLEL_BASE_URL_DEFAULT.to_string(),
            format!("{}/mcp", server.uri()),
            "advanced".to_string(),
        );
        let out = backend
            .search("rust async", 5, None)
            .await
            .expect("parallel keyless search");
        let json: Value = serde_json::from_str(&out).expect("json output");
        assert_eq!(json["provider"], "parallel");
        assert_eq!(json["results"][0]["url"], "https://rust-lang.org");
        assert_eq!(json["attribution"], PARALLEL_FREE_SEARCH_ATTRIBUTION);
    }

    #[tokio::test]
    async fn parallel_keyed_rest_search_posts_v1_payload() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/search"))
            .and(header("authorization", "Bearer parallel-key"))
            .and(body_partial_json(json!({
                "search_queries": ["rust async"],
                "objective": "rust async",
                "mode": "basic",
                "advanced_settings": {"max_results": 3},
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"url": "https://example.com", "title": "Example", "description": "desc"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ParallelWebBackend::with_endpoints(
            Some("parallel-key".to_string()),
            server.uri(),
            PARALLEL_MCP_SEARCH_URL_DEFAULT.to_string(),
            "basic".to_string(),
        );
        let out = backend
            .search("rust async", 3, None)
            .await
            .expect("parallel keyed search");
        let json: Value = serde_json::from_str(&out).expect("json output");
        assert_eq!(json["provider"], "parallel");
        assert_eq!(json["results"][0]["title"], "Example");
        assert!(json.get("attribution").is_none());
    }

    #[test]
    fn xai_json_results_parse_and_renumber_valid_rows() {
        let rows = parse_xai_json_results(
            r#"prefix {"results":[{"title":"A","url":"","description":"drop"},{"title":"B","url":"https://b.example","description":"keep"}]} suffix"#,
            10,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["position"], 1);
        assert_eq!(rows[0]["url"], "https://b.example");
    }

    #[test]
    fn xai_parse_results_falls_back_to_citations() {
        let rows = XaiWebSearchBackend::parse_results(
            &json!({"citations": ["https://one.example", "https://two.example"]}),
            1,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["url"], "https://one.example");
    }
}

#[cfg(test)]
mod firecrawl_managed_tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;
    use serde_json::json;

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
                "FIRECRAWL_API_URL",
                "PARALLEL_API_KEY",
                "PARALLEL_BASE_URL",
                "PARALLEL_MCP_URL",
                "PARALLEL_SEARCH_MODE",
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

    fn write_auth_json(home: &std::path::Path, payload: serde_json::Value) {
        std::fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&payload).expect("auth json serializes"),
        )
        .expect("write auth.json");
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
    fn availability_accepts_expired_cached_nous_token_without_refresh() {
        let scope = EnvScope::new();
        write_auth_json(
            scope._tmp.path(),
            json!({
                "providers": {"nous": {
                    "access_token": "expired-but-present",
                    "expires_at": "2000-01-01T00:00:00Z",
                }}
            }),
        );
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");

        assert!(firecrawl_managed_config_present());
        assert_eq!(search_backend_choice_from_env(), "firecrawl");
        assert_eq!(extract_backend_choice_from_env(), "firecrawl");
    }

    #[test]
    fn from_env_or_managed_errors_when_neither_configured() {
        let _g = EnvScope::new();
        let err = FirecrawlExtractBackend::from_env_or_managed().unwrap_err();
        assert!(err.to_string().contains("FIRECRAWL_API_KEY"));
        assert!(err.to_string().contains("firecrawl gateway"));
    }

    #[test]
    fn from_env_or_managed_accepts_self_hosted_url_without_key() {
        let _g = EnvScope::new();
        std::env::set_var("FIRECRAWL_API_URL", "http://127.0.0.1:3002/v1/");
        let b = FirecrawlExtractBackend::from_env_or_managed().unwrap();
        assert_eq!(b.transport_label(), "direct");
        match &b.transport {
            FirecrawlTransport::Direct {
                endpoint_root,
                api_key,
            } => {
                assert_eq!(endpoint_root, "http://127.0.0.1:3002");
                assert!(api_key.is_none());
                assert_eq!(
                    b.transport.endpoint("scrape"),
                    "http://127.0.0.1:3002/v1/scrape"
                );
            }
            _ => panic!("expected direct transport"),
        }
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
                    b.transport.endpoint("scrape"),
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
