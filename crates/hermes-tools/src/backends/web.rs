//! Real web tool backends: Exa/Tavily/Firecrawl/xAI/SearXNG/Brave/DDG search,
//! Firecrawl/Tavily extract, Tavily crawl, and local fallbacks.

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;
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
const DDG_SEARCH_TIMEOUT_SECS_DEFAULT: u64 = 15;
const PARALLEL_BASE_URL_DEFAULT: &str = "https://api.parallel.ai";
const PARALLEL_USER_AGENT: &str = "hermes-agent-web/1.0.0";
const FIRECRAWL_BASE_URL_DEFAULT: &str = "https://api.firecrawl.dev";
const XAI_BASE_URL_DEFAULT: &str = "https://api.x.ai/v1";
const XAI_WEB_MODEL_DEFAULT: &str = "grok-build-0.1";
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct SourceQuality {
    label: &'static str,
    score: f64,
    reason: &'static str,
}

fn host_matches(host: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| {
        host == *suffix
            || host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn source_quality_for_url(raw_url: &str) -> SourceQuality {
    let url = raw_url.trim();
    if url.is_empty() {
        return SourceQuality {
            label: "secondary",
            score: 0.20,
            reason: "missing URL",
        };
    }

    let parsed = Url::parse(url).or_else(|_| Url::parse(&format!("https://{url}")));
    let Ok(parsed) = parsed else {
        return SourceQuality {
            label: "secondary",
            score: 0.35,
            reason: "unparseable URL",
        };
    };
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = parsed.path().to_ascii_lowercase();
    let host_and_path = format!("{host}{path}");

    if host_matches(&host, &["github.com", "gitlab.com"])
        && contains_any(&path, &["/issues/", "/discussions/", "/pull/"])
    {
        return SourceQuality {
            label: "community",
            score: 0.78,
            reason: "repository discussion",
        };
    }
    if host_matches(
        &host,
        &[
            "reddit.com",
            "old.reddit.com",
            "news.ycombinator.com",
            "stackoverflow.com",
            "stackexchange.com",
        ],
    ) || host.contains("forum")
        || host.contains("discourse")
    {
        return SourceQuality {
            label: "community",
            score: 0.74,
            reason: "expert/community discussion",
        };
    }
    if host_matches(
        &host,
        &[
            "github.com",
            "gitlab.com",
            "bitbucket.org",
            "docs.rs",
            "crates.io",
            "rfc-editor.org",
            "ietf.org",
            "w3.org",
            "arxiv.org",
            "doi.org",
        ],
    ) {
        return SourceQuality {
            label: "primary",
            score: 0.96,
            reason: "source repository, registry, standard, or paper",
        };
    }
    if host.ends_with(".gov")
        || host.ends_with(".mil")
        || host.ends_with(".edu")
        || host.starts_with("docs.")
        || host.starts_with("doc.")
        || host.starts_with("developer.")
        || contains_any(
            &host_and_path,
            &[
                "/docs",
                "/documentation",
                "/reference",
                "/api/",
                "/sdk",
                "/spec",
                "/protocol",
                "/whitepaper",
                "/manual",
            ],
        )
    {
        return SourceQuality {
            label: "primary",
            score: 0.90,
            reason: "official documentation or institutional source",
        };
    }
    if host_matches(
        &host,
        &[
            "medium.com",
            "substack.com",
            "youtube.com",
            "youtu.be",
            "forbes.com",
            "cointelegraph.com",
            "decrypt.co",
        ],
    ) || contains_any(&path, &["/blog/", "/news/", "/article/", "/posts/"])
    {
        return SourceQuality {
            label: "secondary",
            score: 0.42,
            reason: "article or summary source",
        };
    }

    SourceQuality {
        label: "secondary",
        score: 0.50,
        reason: "general web result",
    }
}

fn enrich_source_quality(row: &mut Value, fallback_position: usize) {
    let url = row.get("url").and_then(Value::as_str).unwrap_or_default();
    let quality = source_quality_for_url(url);
    let original_position = row
        .get("position")
        .and_then(Value::as_u64)
        .filter(|position| *position > 0)
        .unwrap_or(fallback_position as u64);
    if let Some(obj) = row.as_object_mut() {
        obj.insert("source_quality".to_string(), json!(quality.label));
        obj.insert("source_quality_score".to_string(), json!(quality.score));
        obj.insert("source_quality_reason".to_string(), json!(quality.reason));
        obj.insert("original_position".to_string(), json!(original_position));
    }
}

fn ranked_search_results(mut rows: Vec<Value>) -> Vec<Value> {
    for (idx, row) in rows.iter_mut().enumerate() {
        enrich_source_quality(row, idx + 1);
    }
    rows.sort_by(|left, right| {
        let left_quality = left
            .get("source_quality_score")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let right_quality = right
            .get("source_quality_score")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let left_score = left.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let right_score = right.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let left_position = left
            .get("original_position")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let right_position = right
            .get("original_position")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);

        right_quality
            .partial_cmp(&left_quality)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right_score
                    .partial_cmp(&left_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left_position.cmp(&right_position))
    });
    for (idx, row) in rows.iter_mut().enumerate() {
        row["position"] = json!(idx + 1);
        row["source_rank"] = json!(idx + 1);
    }
    rows
}

fn source_quality_summary(rows: &[Value]) -> Value {
    let mut primary = 0usize;
    let mut community = 0usize;
    let mut secondary = 0usize;
    for row in rows {
        match row.get("source_quality").and_then(Value::as_str) {
            Some("primary") => primary += 1,
            Some("community") => community += 1,
            _ => secondary += 1,
        }
    }
    json!({
        "primary": primary,
        "community": community,
        "secondary": secondary,
    })
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
        let quality = source_quality_for_url(url);
        Ok(json!({
            "success": false,
            "error": format!("{} is a search-only web backend and cannot extract URLs", self.provider),
            "url": url,
            "provider": self.provider,
            "source_quality": quality.label,
            "source_quality_score": quality.score,
            "source_quality_reason": quality.reason,
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
            let quality = source_quality_for_url(url);
            let result = json!({
                "url": url,
                "content_type": content_type,
                "content": redact_web_content(&text),
                "truncated": true,
                "original_size": bytes.len(),
                "source_quality": quality.label,
                "source_quality_score": quality.score,
                "source_quality_reason": quality.reason,
            });
            return serde_json::to_string_pretty(&result)
                .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {}", e)));
        }

        let text = String::from_utf8_lossy(&bytes);

        let content = redact_web_content(&strip_html_tags(&text));

        let quality = source_quality_for_url(url);
        let result = json!({
            "url": url,
            "content_type": content_type,
            "content": content,
            "truncated": false,
            "size": bytes.len(),
            "source_quality": quality.label,
            "source_quality_score": quality.score,
            "source_quality_reason": quality.reason,
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
        let formatted: Vec<Value> = ranked_search_results(
            results
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .map(|(idx, r)| {
                            search_result(
                                r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                                r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                                r.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                                r.get("score").and_then(|v| v.as_f64()),
                                idx + 1,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );

        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "source_quality_summary": source_summary,
        }))
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
        let formatted: Vec<Value> = ranked_search_results(
            results
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .map(|(idx, r)| {
                            search_result(
                                r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                                r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                                r.get("content")
                                    .or_else(|| r.get("raw_content"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(""),
                                r.get("score").and_then(|v| v.as_f64()),
                                idx + 1,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );

        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "source_quality_summary": source_summary,
        }))
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
        let source_summary = source_quality_summary(&documents);
        let result = json!({
            "url": url,
            "content": first_content,
            "results": documents,
            "provider": "tavily",
            "source_quality_summary": source_summary,
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
        let documents = normalize_tavily_documents(&data, url);
        let source_summary = source_quality_summary(&documents);
        let result = json!({
            "url": url,
            "results": documents,
            "provider": "tavily",
            "source_quality_summary": source_summary,
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
    let mut row = json!({
        "title": title,
        "url": url,
        "description": description,
        "text": description,
        "score": score.unwrap_or(0.0),
        "position": position,
    });
    enrich_source_quality(&mut row, position);
    row
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
                "source_quality": source_quality_for_url(url).label,
                "source_quality_score": source_quality_for_url(url).score,
                "source_quality_reason": source_quality_for_url(url).reason,
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
                "source_quality": source_quality_for_url(url).label,
                "source_quality_score": source_quality_for_url(url).score,
                "source_quality_reason": source_quality_for_url(url).reason,
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
                "source_quality": source_quality_for_url(&url).label,
                "source_quality_score": source_quality_for_url(&url).score,
                "source_quality_reason": source_quality_for_url(&url).reason,
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
        let rows: Vec<Value> = data
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
        let formatted: Vec<Value> = ranked_search_results(rows)
            .into_iter()
            .take(num_results)
            .collect();

        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "source_quality_summary": source_summary,
        }))
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
        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "provider": "brave-free",
            "source_quality_summary": source_summary,
        }))
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {e}")))
    }
}

fn normalize_brave_results(response: &Value, limit: usize) -> Vec<Value> {
    let rows: Vec<Value> = response
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(|results| results.as_array())
        .map(|rows| {
            rows.iter()
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
        .unwrap_or_default();
    ranked_search_results(rows)
        .into_iter()
        .take(limit)
        .collect()
}

// ---------------------------------------------------------------------------
// DuckDuckGoSearchBackend
// ---------------------------------------------------------------------------

/// Keyless DuckDuckGo Instant Answer backend.
pub struct DuckDuckGoSearchBackend {
    client: Client,
    endpoint: String,
    timeout: Duration,
}

impl DuckDuckGoSearchBackend {
    pub fn new(endpoint: String) -> Self {
        Self::with_timeout(
            endpoint,
            Duration::from_secs(DDG_SEARCH_TIMEOUT_SECS_DEFAULT),
        )
    }

    pub fn with_timeout(endpoint: String, timeout: Duration) -> Self {
        Self {
            client: Client::new(),
            endpoint,
            timeout: timeout.max(Duration::from_millis(50)),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let endpoint = std::env::var("DDG_SEARCH_URL")
            .unwrap_or_else(|_| DDG_INSTANT_ANSWER_URL_DEFAULT.to_string());
        let timeout = env_optional_nonempty("DDG_SEARCH_TIMEOUT_SECONDS")
            .or_else(|| env_optional_nonempty("HERMES_DDGS_TIMEOUT_SECONDS"))
            .and_then(|raw| raw.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or_else(|| Duration::from_secs(DDG_SEARCH_TIMEOUT_SECS_DEFAULT));
        Ok(Self::with_timeout(endpoint, timeout))
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
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
            .timeout(self.timeout)
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
        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "provider": "ddgs",
            "source_quality_summary": source_summary,
        }))
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
    ranked_search_results(rows)
        .into_iter()
        .take(limit)
        .collect()
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

include!("web/parallel_xai_firecrawl.rs");

include!("web/web_search_env_tests.rs");
