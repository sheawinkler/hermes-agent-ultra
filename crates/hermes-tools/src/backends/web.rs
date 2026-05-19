//! Real web tool backends: Exa/Tavily/SearXNG search, Firecrawl extract, and local fallbacks.

use async_trait::async_trait;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::time::Instant;
use tracing::{debug, trace};

use crate::tools::web::{WebExtractBackend, WebSearchBackend};
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use hermes_core::ToolError;

// ---------------------------------------------------------------------------
// FallbackSearchBackend (no API key needed)
// ---------------------------------------------------------------------------

const FALLBACK_SEARCH_LIMIT: usize = 20;
const FALLBACK_DDG_LITE_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";
const FALLBACK_DDG_INSTANT_ENDPOINT: &str = "https://api.duckduckgo.com/";
const FALLBACK_WIKIPEDIA_OPENSEARCH_ENDPOINT: &str = "https://en.wikipedia.org/w/api.php";
const FALLBACK_HN_ALGOLIA_ENDPOINT: &str = "https://hn.algolia.com/api/v1/search";

/// Free search fallback for users without paid web-search API keys.
pub struct FallbackSearchBackend {
    client: Client,
}

impl FallbackSearchBackend {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .user_agent("hermes-agent/1.0")
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
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
        num_results: usize,
        _category: Option<&str>,
    ) -> Result<String, ToolError> {
        let limit = clamp_fallback_limit(num_results);
        let mut attempts = Vec::new();
        debug!(
            query_chars = query.chars().count(),
            limit,
            "web_search fallback chain start"
        );

        for source in ["ddg_lite", "instant", "vertical"] {
            let attempt_start = Instant::now();
            let result = match source {
                "ddg_lite" => self.search_ddg_lite(query, limit).await,
                "instant" => self.search_ddg_instant(query, limit).await,
                "vertical" => self.search_vertical(query, limit).await,
                _ => unreachable!(),
            };
            let elapsed_ms = attempt_start.elapsed().as_millis() as u64;

            match result {
                Ok(rows) if !rows.is_empty() => {
                    debug!(
                        backend = source,
                        elapsed_ms,
                        count = rows.len(),
                        "web_search fallback backend selected"
                    );
                    attempts.push(json!({
                        "backend": source,
                        "status": "ok",
                        "duration_ms": elapsed_ms,
                        "count": rows.len(),
                    }));
                    return serde_json::to_string_pretty(&json!({
                        "query": query,
                        "results": rows,
                        "count": rows.len(),
                        "selected_backend": source,
                        "_trace": { "attempts": attempts },
                    }))
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("Failed to serialize results: {}", e))
                    });
                }
                Ok(rows) => {
                    debug!(
                        backend = source,
                        elapsed_ms,
                        count = rows.len(),
                        "web_search fallback backend returned empty"
                    );
                    attempts.push(json!({
                        "backend": source,
                        "status": "empty",
                        "duration_ms": elapsed_ms,
                        "count": rows.len(),
                    }));
                }
                Err(err) => {
                    debug!(
                        backend = source,
                        elapsed_ms,
                        error = %err,
                        "web_search fallback backend failed"
                    );
                    attempts.push(json!({
                        "backend": source,
                        "status": "error",
                        "duration_ms": elapsed_ms,
                        "error": truncate_fallback_text(&err.to_string(), 250),
                    }));
                }
            }
        }

        serde_json::to_string_pretty(&json!({
            "query": query,
            "results": [],
            "count": 0,
            "selected_backend": null,
            "_trace": { "attempts": attempts },
            "message": "No configured API key was found, and free fallback search returned no results.",
        }))
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {}", e)))
    }
}

impl FallbackSearchBackend {
    async fn search_ddg_lite(&self, query: &str, limit: usize) -> Result<Vec<Value>, ToolError> {
        let mut url = Url::parse(FALLBACK_DDG_LITE_ENDPOINT).map_err(|e| {
            ToolError::ExecutionFailed(format!("Invalid DuckDuckGo Lite endpoint: {}", e))
        })?;
        url.query_pairs_mut().append_pair("q", query);

        let html = self.fetch_text(url).await?;
        Ok(unique_and_limit_results(
            parse_duckduckgo_lite_results(&html),
            limit,
        ))
    }

    async fn search_ddg_instant(&self, query: &str, limit: usize) -> Result<Vec<Value>, ToolError> {
        let mut url = Url::parse(FALLBACK_DDG_INSTANT_ENDPOINT).map_err(|e| {
            ToolError::ExecutionFailed(format!("Invalid DuckDuckGo instant endpoint: {}", e))
        })?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("format", "json")
            .append_pair("no_html", "1")
            .append_pair("no_redirect", "1")
            .append_pair("skip_disambig", "1");

        let text = self.fetch_text(url).await?;
        let payload: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse DuckDuckGo instant JSON: {}", e))
        })?;

        let mut rows = Vec::new();
        let heading = value_str(payload.get("Heading")).unwrap_or(query);
        if let Some(abstract_url) = value_str(payload.get("AbstractURL")).filter(|u| is_http_url(u))
        {
            rows.push(json!({
                "title": if heading.is_empty() { query } else { heading },
                "url": abstract_url,
                "text": truncate_fallback_text(value_str(payload.get("AbstractText")).unwrap_or(""), 500),
                "source": "instant",
            }));
        }

        if let Some(results) = payload.get("Results").and_then(Value::as_array) {
            for item in results {
                if let Some(row) = normalize_fallback_result(item, "instant", query) {
                    rows.push(row);
                }
            }
        }
        if let Some(related) = payload.get("RelatedTopics").and_then(Value::as_array) {
            append_instant_related_rows(related, &mut rows, query);
        }

        Ok(unique_and_limit_results(rows, limit))
    }

    async fn search_vertical(&self, query: &str, limit: usize) -> Result<Vec<Value>, ToolError> {
        let (wiki, wiki_err) = match self.search_wikipedia(query, limit).await {
            Ok(rows) => (rows, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        let (hn, hn_err) = match self.search_hacker_news(query, limit).await {
            Ok(rows) => (rows, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };

        let rows = unique_and_limit_results(wiki.into_iter().chain(hn).collect(), limit);
        if rows.is_empty() {
            if let (Some(wiki), Some(hn)) = (wiki_err, hn_err) {
                return Err(ToolError::ExecutionFailed(format!(
                    "Wikipedia search failed: {}; Hacker News search failed: {}",
                    wiki, hn
                )));
            }
        }
        Ok(rows)
    }

    async fn search_wikipedia(&self, query: &str, limit: usize) -> Result<Vec<Value>, ToolError> {
        let mut url = Url::parse(FALLBACK_WIKIPEDIA_OPENSEARCH_ENDPOINT).map_err(|e| {
            ToolError::ExecutionFailed(format!("Invalid Wikipedia endpoint: {}", e))
        })?;
        url.query_pairs_mut()
            .append_pair("action", "opensearch")
            .append_pair("search", query)
            .append_pair("limit", &limit.to_string())
            .append_pair("namespace", "0")
            .append_pair("format", "json");

        let text = self.fetch_text(url).await?;
        let payload: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Wikipedia JSON: {}", e))
        })?;
        let Some(items) = payload.as_array() else {
            return Ok(Vec::new());
        };
        let titles = items.get(1).and_then(Value::as_array);
        let descriptions = items.get(2).and_then(Value::as_array);
        let urls = items.get(3).and_then(Value::as_array);

        let mut rows = Vec::new();
        if let Some(urls) = urls {
            for (i, target) in urls.iter().enumerate() {
                let Some(target) = value_str(Some(target)).filter(|u| is_http_url(u)) else {
                    continue;
                };
                let title = titles
                    .and_then(|v| v.get(i))
                    .and_then(|v| value_str(Some(v)))
                    .filter(|v| !v.is_empty())
                    .unwrap_or(query);
                let text = descriptions
                    .and_then(|v| v.get(i))
                    .and_then(|v| value_str(Some(v)))
                    .unwrap_or("");
                rows.push(json!({
                    "title": title,
                    "url": target,
                    "text": truncate_fallback_text(text, 500),
                    "source": "vertical_wikipedia",
                }));
            }
        }
        Ok(unique_and_limit_results(rows, limit))
    }

    async fn search_hacker_news(&self, query: &str, limit: usize) -> Result<Vec<Value>, ToolError> {
        let mut url = Url::parse(FALLBACK_HN_ALGOLIA_ENDPOINT).map_err(|e| {
            ToolError::ExecutionFailed(format!("Invalid Hacker News endpoint: {}", e))
        })?;
        url.query_pairs_mut()
            .append_pair("query", query)
            .append_pair("tags", "story")
            .append_pair("hitsPerPage", &limit.to_string());

        let text = self.fetch_text(url).await?;
        let payload: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Hacker News JSON: {}", e))
        })?;
        let mut rows = Vec::new();
        if let Some(hits) = payload.get("hits").and_then(Value::as_array) {
            for hit in hits {
                let target = first_value_str(hit, &["url", "story_url"]);
                let Some(target) = target.filter(|u| is_http_url(u)) else {
                    continue;
                };
                let title = first_value_str(hit, &["title", "story_title"])
                    .filter(|v| !v.is_empty())
                    .unwrap_or(query);
                let author = value_str(hit.get("author")).unwrap_or("");
                let points = hit.get("points").and_then(Value::as_i64).unwrap_or(0);
                rows.push(json!({
                    "title": title,
                    "url": target,
                    "text": format!("HN by {}, points: {}", author, points),
                    "source": "vertical_hn",
                }));
            }
        }
        Ok(unique_and_limit_results(rows, limit))
    }

    async fn fetch_text(&self, url: Url) -> Result<String, ToolError> {
        let started = Instant::now();
        trace!(host = ?url.host_str(), path = %url.path(), "web_search fallback HTTP request start");
        let resp = self.client.get(url.clone()).send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Fallback search request failed: {}", e))
        })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read fallback search response: {}", e))
        })?;
        trace!(
            host = ?url.host_str(),
            path = %url.path(),
            status = %status,
            elapsed_ms = started.elapsed().as_millis() as u64,
            bytes = text.len(),
            "web_search fallback HTTP request finished"
        );
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Fallback search HTTP {} from {}: {}",
                status,
                url,
                truncate_fallback_text(text.trim(), 240)
            )));
        }
        Ok(text)
    }
}

fn parse_duckduckgo_lite_results(html: &str) -> Vec<Value> {
    let Ok(anchor_re) = Regex::new(r#"(?is)<a[^>]+href=["']([^"']+)["'][^>]*>(.*?)</a>"#) else {
        return Vec::new();
    };

    anchor_re
        .captures_iter(html)
        .filter_map(|captures| {
            let target = normalize_duckduckgo_url(captures.get(1)?.as_str())?;
            let title = strip_html_tags(&decode_basic_html_entities(captures.get(2)?.as_str()));
            if title.is_empty() {
                return None;
            }
            Some(json!({
                "title": title,
                "url": target,
                "text": "",
                "source": "ddg_lite",
            }))
        })
        .collect()
}

fn normalize_duckduckgo_url(raw: &str) -> Option<String> {
    let raw = decode_basic_html_entities(raw).trim().to_string();
    if raw.is_empty() {
        return None;
    }

    let parsed = Url::parse(&raw).or_else(|_| {
        Url::parse("https://duckduckgo.com").and_then(|base| base.join(&raw))
    }).ok()?;

    let mut target = parsed.to_string();
    if parsed
        .host_str()
        .map(|h| h.to_ascii_lowercase().contains("duckduckgo.com"))
        .unwrap_or(false)
        && parsed.path().starts_with("/l/")
    {
        if let Some((_, uddg)) = parsed.query_pairs().find(|(key, _)| key == "uddg") {
            target = uddg.into_owned();
        }
    }

    if !is_http_url(&target) {
        return None;
    }
    let dst = Url::parse(&target).ok()?;
    if dst
        .host_str()
        .map(|h| h.to_ascii_lowercase().contains("duckduckgo.com"))
        .unwrap_or(false)
    {
        return None;
    }
    Some(target)
}

fn append_instant_related_rows(items: &[Value], rows: &mut Vec<Value>, query: &str) {
    for item in items {
        if let Some(nested) = item.get("Topics").and_then(Value::as_array) {
            append_instant_related_rows(nested, rows, query);
            continue;
        }
        if let Some(row) = normalize_fallback_result(item, "instant", query) {
            rows.push(row);
        }
    }
}

fn normalize_fallback_result(item: &Value, source: &str, default_title: &str) -> Option<Value> {
    let target = first_value_str(
        item,
        &[
            "url",
            "href",
            "link",
            "FirstURL",
            "first_url",
            "story_url",
            "AbstractURL",
        ],
    )?;
    if !is_http_url(target) {
        return None;
    }

    let text = first_value_str(
        item,
        &[
            "snippet",
            "body",
            "content",
            "description",
            "text",
            "Text",
            "AbstractText",
        ],
    )
    .unwrap_or("");
    let title = first_value_str(item, &["title", "heading", "name", "story_title", "Heading"])
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(|| {
            if text.is_empty() {
                None
            } else {
                Some(truncate_fallback_text(text, 180))
            }
        })
        .unwrap_or_else(|| default_title.to_string());

    Some(json!({
        "title": title,
        "url": target,
        "text": truncate_fallback_text(text, 500),
        "source": source,
    }))
}

fn first_value_str<'a>(item: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value_str(item.get(*key)).filter(|v| !v.trim().is_empty()))
}

fn value_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim)
}

fn unique_and_limit_results(rows: Vec<Value>, limit: usize) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        if out.len() >= limit {
            break;
        }
        let Some(url) = row.get("url").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        let title = row.get("title").and_then(Value::as_str).unwrap_or("").trim();
        let key = format!("{}|{}", url.to_ascii_lowercase(), title.to_ascii_lowercase());
        if seen.insert(key) {
            out.push(row);
        }
    }
    out
}

fn clamp_fallback_limit(limit: usize) -> usize {
    match limit {
        0 => 5,
        n if n > FALLBACK_SEARCH_LIMIT => FALLBACK_SEARCH_LIMIT,
        n => n,
    }
}

fn is_http_url(raw: &str) -> bool {
    Url::parse(raw)
        .map(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        .unwrap_or(false)
}

fn truncate_fallback_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn decode_basic_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

// ---------------------------------------------------------------------------
// SimpleExtractBackend (uses reqwest, no API key needed)
// ---------------------------------------------------------------------------

const MAX_EXTRACT_BYTES: usize = 512_000; // 500 KB
const TAVILY_BASE_URL_DEFAULT: &str = "https://api.tavily.com";
const SEARXNG_SEARCH_PATH: &str = "/search";

/// Chrome-like User-Agent for direct HTML fetches (many sites block bot UAs with 403).
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

const BROWSER_ACCEPT_HTML: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

fn build_browser_like_http_client(timeout_secs: u64) -> Client {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(BROWSER_ACCEPT_HTML),
    );
    Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .user_agent(BROWSER_USER_AGENT)
        .default_headers(headers)
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// A web extraction backend that fetches HTML via reqwest with no external API dependency.
pub struct SimpleExtractBackend {
    client: Client,
}

impl SimpleExtractBackend {
    pub fn new() -> Self {
        Self {
            client: build_browser_like_http_client(30),
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
        let started = Instant::now();
        debug!(
            backend = "exa",
            query_chars = query.chars().count(),
            num_results,
            category = ?category,
            "web_search backend request start"
        );
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
        debug!(
            backend = "exa",
            status = %status,
            elapsed_ms = started.elapsed().as_millis() as u64,
            bytes = text.len(),
            "web_search backend response received"
        );

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
        let started = Instant::now();
        debug!(
            backend = "tavily",
            query_chars = query.chars().count(),
            num_results,
            category = ?category,
            "web_search backend request start"
        );
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
        debug!(
            backend = "tavily",
            status = %status,
            elapsed_ms = started.elapsed().as_millis() as u64,
            bytes = text.len(),
            "web_search backend response received"
        );

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

    /// Create from `SEARXNG_BASE_URL`.
    pub fn from_env() -> Result<Self, ToolError> {
        let base_url = std::env::var("SEARXNG_BASE_URL").map_err(|_| {
            ToolError::ExecutionFailed("SEARXNG_BASE_URL environment variable not set".into())
        })?;
        let base_url = base_url.trim();
        if base_url.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "SEARXNG_BASE_URL environment variable is empty".into(),
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
        let started = Instant::now();
        debug!(
            backend = "searxng",
            query_chars = query.chars().count(),
            num_results,
            category = ?category,
            "web_search backend request start"
        );
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
        debug!(
            backend = "searxng",
            status = %status,
            elapsed_ms = started.elapsed().as_millis() as u64,
            bytes = text.len(),
            "web_search backend response received"
        );

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "SearXNG API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse SearXNG response: {}", e))
        })?;
        let formatted: Vec<Value> = data
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .take(num_results)
                    .map(|r| {
                        json!({
                            "title": r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                            "url": r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                            "text": r
                                .get("content")
                                .or_else(|| r.get("snippet"))
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
/// 1. Explicit `HERMES_WEB_SEARCH_BACKEND` override
///    - `exa`, `tavily`, `searxng`, `fallback`/`duckduckgo`
/// 2. Exa (`EXA_API_KEY`)
/// 3. Tavily (`TAVILY_API_KEY`, optional `TAVILY_BASE_URL`)
/// 4. SearXNG (`SEARXNG_BASE_URL`)
/// 5. Free fallback chain (DuckDuckGo Lite, DuckDuckGo Instant, vertical search)
pub fn search_backend_from_env_or_fallback() -> Box<dyn WebSearchBackend> {
    let choice = search_backend_choice_from_env();
    debug!(backend = choice, "web_search backend resolved from environment");
    match choice {
        "exa" => ExaSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "tavily" => TavilySearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        "searxng" => SearxngSearchBackend::from_env()
            .map(|b| Box::new(b) as Box<dyn WebSearchBackend>)
            .unwrap_or_else(|_| Box::new(FallbackSearchBackend::new())),
        _ => Box::new(FallbackSearchBackend::new()),
    }
}

fn search_backend_choice_from_env() -> &'static str {
    if let Ok(choice) = std::env::var("HERMES_WEB_SEARCH_BACKEND") {
        match choice.trim().to_ascii_lowercase().as_str() {
            "exa" => return "exa",
            "tavily" => return "tavily",
            "searxng" | "searx" => return "searxng",
            "fallback" | "duckduckgo" | "ddg" | "free" | "none" | "off" | "disabled" => {
                return "fallback";
            }
            _ => {}
        }
    }

    if env_present_nonempty("EXA_API_KEY") {
        "exa"
    } else if env_present_nonempty("TAVILY_API_KEY") {
        "tavily"
    } else if env_present_nonempty("SEARXNG_BASE_URL") {
        "searxng"
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
mod browser_client_tests {
    use super::*;

    #[test]
    fn browser_user_agent_looks_like_chrome() {
        assert!(BROWSER_USER_AGENT.contains("Chrome/"));
        assert!(BROWSER_ACCEPT_HTML.starts_with("text/html,application/xhtml+xml"));
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
                "SEARXNG_BASE_URL",
                "HERMES_WEB_SEARCH_BACKEND",
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

    #[test]
    fn searxng_from_env_normalizes_base_url() {
        let _scope = EnvScope::new();
        std::env::set_var("SEARXNG_BASE_URL", "https://search.example.com/");
        let backend = SearxngSearchBackend::from_env().expect("searxng backend from env");
        assert_eq!(backend.base_url(), "https://search.example.com");
    }

    #[test]
    fn search_backend_choice_uses_searxng_when_only_base_url_available() {
        let _scope = EnvScope::new();
        std::env::set_var("SEARXNG_BASE_URL", "https://search.example.com");
        assert_eq!(search_backend_choice_from_env(), "searxng");
    }

    #[test]
    fn search_backend_choice_honors_explicit_override() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "searxng");
        std::env::set_var("EXA_API_KEY", "exa-key");
        assert_eq!(search_backend_choice_from_env(), "searxng");
    }

    #[test]
    fn search_backend_choice_falls_back_when_keys_missing() {
        let _scope = EnvScope::new();
        assert_eq!(search_backend_choice_from_env(), "fallback");
    }

    #[test]
    fn search_backend_choice_accepts_duckduckgo_override() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "duckduckgo");
        assert_eq!(search_backend_choice_from_env(), "fallback");
    }

    #[test]
    fn parse_duckduckgo_lite_results_extracts_redirect_target() {
        let html = r#"
            <a rel="nofollow" href="/l/?uddg=https%3A%2F%2Fexample.com%2Fdoc">Example &amp; Docs</a>
            <a href="https://duckduckgo.com/about">DuckDuckGo</a>
        "#;

        let rows = parse_duckduckgo_lite_results(html);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], "Example & Docs");
        assert_eq!(rows[0]["url"], "https://example.com/doc");
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
