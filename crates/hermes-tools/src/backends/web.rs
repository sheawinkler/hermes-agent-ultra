//! Real web tool backends: Exa search, Firecrawl extract, and local fallbacks.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use hermes_core::ToolError;
use crate::tools::web::{WebSearchBackend, WebExtractBackend};

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
    async fn search(&self, query: &str, _num_results: usize, _category: Option<&str>) -> Result<String, ToolError> {
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
        let resp = self.client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to fetch '{}': {}", url, e)))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "HTTP {} when fetching '{}'", status, url
            )));
        }

        let content_type = resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = resp.bytes().await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read response body: {}", e)))?;

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
        let api_key = std::env::var("EXA_API_KEY")
            .map_err(|_| ToolError::ExecutionFailed("EXA_API_KEY environment variable not set".into()))?;
        Ok(Self::new(api_key))
    }
}

#[async_trait]
impl WebSearchBackend for ExaSearchBackend {
    async fn search(&self, query: &str, num_results: usize, category: Option<&str>) -> Result<String, ToolError> {
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

        let resp = self.client
            .post("https://api.exa.ai/search")
            .header("x-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Exa API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read Exa response: {}", e)))?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!("Exa API error ({}): {}", status, text)));
        }

        // Parse and reformat the response
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse Exa response: {}", e)))?;

        let results = data.get("results").and_then(|r| r.as_array());
        let formatted: Vec<Value> = results
            .map(|arr| {
                arr.iter().map(|r| {
                    json!({
                        "title": r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                        "url": r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                        "text": r.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                        "score": r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    })
                }).collect()
            })
            .unwrap_or_default();

        serde_json::to_string_pretty(&json!({ "results": formatted }))
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize results: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// FirecrawlExtractBackend
// ---------------------------------------------------------------------------

/// Real Firecrawl API extract backend.
pub struct FirecrawlExtractBackend {
    client: Client,
    api_key: String,
}

impl FirecrawlExtractBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }

    /// Create from environment variable `FIRECRAWL_API_KEY`.
    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = std::env::var("FIRECRAWL_API_KEY")
            .map_err(|_| ToolError::ExecutionFailed("FIRECRAWL_API_KEY environment variable not set".into()))?;
        Ok(Self::new(api_key))
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

        let resp = self.client
            .post("https://api.firecrawl.dev/v1/scrape")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Firecrawl API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read Firecrawl response: {}", e)))?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!("Firecrawl API error ({}): {}", status, text)));
        }

        let data: Value = serde_json::from_str(&text)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse Firecrawl response: {}", e)))?;

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
        });

        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize result: {}", e)))
    }
}
