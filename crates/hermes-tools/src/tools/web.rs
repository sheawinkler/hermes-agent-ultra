//! Web tools: web_search (Exa) and web_extract (Firecrawl)

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// Backend traits (injectable by the caller)
// ---------------------------------------------------------------------------

/// Backend for web search operations (e.g. Exa API).
#[async_trait]
pub trait WebSearchBackend: Send + Sync {
    /// Search the web and return results as JSON string.
    async fn search(
        &self,
        query: &str,
        num_results: usize,
        category: Option<&str>,
    ) -> Result<String, ToolError>;
}

/// Backend for web extraction operations (e.g. Firecrawl API).
#[async_trait]
pub trait WebExtractBackend: Send + Sync {
    /// Extract content from a URL and return results as a string.
    async fn extract(&self, url: &str, include_links: bool) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// WebSearchHandler
// ---------------------------------------------------------------------------

/// Web search tool using Exa API.
pub struct WebSearchHandler {
    backend: Box<dyn WebSearchBackend>,
}

impl WebSearchHandler {
    pub fn new(backend: Box<dyn WebSearchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WebSearchHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'query' parameter".into()))?;

        let num_results = params
            .get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let category = params.get("category").and_then(|v| v.as_str());

        self.backend.search(query, num_results, category).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "query".into(),
            json!({
                "type": "string",
                "description": "The search query"
            }),
        );
        props.insert(
            "num_results".into(),
            json!({
                "type": "integer",
                "description": "Number of results to return (default: 10)",
                "default": 10
            }),
        );
        props.insert("category".into(), json!({
            "type": "string",
            "description": "Optional category filter (e.g. 'research paper', 'news', 'github')",
            "enum": ["research paper", "news", "github", "tweet", "movie", "song", "personal site", "pdf"]
        }));

        tool_schema(
            "web_search",
            "Search the web using Exa API. Returns relevant results with titles, URLs, and snippets.",
            JsonSchema::object(props, vec!["query".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// WebExtractHandler
// ---------------------------------------------------------------------------

/// Web extraction tool using Firecrawl API.
pub struct WebExtractHandler {
    backend: Box<dyn WebExtractBackend>,
}

impl WebExtractHandler {
    pub fn new(backend: Box<dyn WebExtractBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WebExtractHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?;

        let include_links = params
            .get("include_links")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        self.backend.extract(url, include_links).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "The URL to extract content from"
            }),
        );
        props.insert(
            "include_links".into(),
            json!({
                "type": "boolean",
                "description": "Whether to include links found on the page (default: true)",
                "default": true
            }),
        );

        tool_schema(
            "web_extract",
            "Extract clean content from a web page using Firecrawl. Returns the page text and optional links.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSearchBackend;
    #[async_trait]
    impl WebSearchBackend for MockSearchBackend {
        async fn search(
            &self,
            query: &str,
            num_results: usize,
            _category: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Results for '{}' (count: {})", query, num_results))
        }
    }

    struct MockExtractBackend;
    #[async_trait]
    impl WebExtractBackend for MockExtractBackend {
        async fn extract(&self, url: &str, _include_links: bool) -> Result<String, ToolError> {
            Ok(format!("Content from {}", url))
        }
    }

    #[tokio::test]
    async fn test_web_search_schema() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "web_search");
        assert!(schema.parameters.properties.is_some());
    }

    #[tokio::test]
    async fn test_web_search_execute() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let result = handler
            .execute(json!({"query": "rust async"}))
            .await
            .unwrap();
        assert!(result.contains("rust async"));
    }

    #[tokio::test]
    async fn test_web_extract_schema() {
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "web_extract");
    }

    #[tokio::test]
    async fn test_web_extract_execute() {
        let handler = WebExtractHandler::new(Box::new(MockExtractBackend));
        let result = handler
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(result.contains("example.com"));
    }

    #[tokio::test]
    async fn test_web_search_missing_params() {
        let handler = WebSearchHandler::new(Box::new(MockSearchBackend));
        let result = handler.execute(json!({})).await;
        assert!(result.is_err());
    }
}
