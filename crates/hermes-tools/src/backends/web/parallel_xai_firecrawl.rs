// ---------------------------------------------------------------------------
// ParallelWebBackend
// ---------------------------------------------------------------------------

/// Parallel.ai search/extract backend.
///
/// Uses Parallel's v1 REST endpoints and requires `PARALLEL_API_KEY`.
pub struct ParallelWebBackend {
    client: Client,
    api_key: Option<String>,
    rest_base_url: String,
    search_mode: String,
}

impl ParallelWebBackend {
    pub fn from_env() -> Self {
        Self::with_endpoints(
            env_optional_nonempty("PARALLEL_API_KEY"),
            env_optional_nonempty("PARALLEL_BASE_URL")
                .unwrap_or_else(|| PARALLEL_BASE_URL_DEFAULT.to_string()),
            parallel_search_mode_from_env(),
        )
    }

    fn with_endpoints(api_key: Option<String>, rest_base_url: String, search_mode: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent(PARALLEL_USER_AGENT)
                .build()
                .unwrap_or_else(|_| Client::new()),
            api_key,
            rest_base_url: rest_base_url.trim().trim_end_matches('/').to_string(),
            search_mode,
        }
    }

    fn new_session_id() -> String {
        format!("parallel-{}", Uuid::new_v4().simple())
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
        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "provider": "parallel",
            "source_quality_summary": source_summary,
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
        parallel_extract_response(url, documents)
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
            .header(reqwest::header::USER_AGENT, PARALLEL_USER_AGENT)
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
            Err(ToolError::ExecutionFailed(
                "PARALLEL_API_KEY is required for the Parallel web search backend".into(),
            ))
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
            Err(ToolError::ExecutionFailed(
                "PARALLEL_API_KEY is required for the Parallel web extract backend".into(),
            ))
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
    let normalized: Vec<Value> = rows
        .map(|rows| {
            rows.iter()
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
        .unwrap_or_default();
    ranked_search_results(normalized)
        .into_iter()
        .take(limit)
        .collect()
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
                "source_quality": source_quality_for_url(url).label,
                "source_quality_score": source_quality_for_url(url).score,
                "source_quality_reason": source_quality_for_url(url).reason,
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
                "source_quality": source_quality_for_url(url).label,
                "source_quality_score": source_quality_for_url(url).score,
                "source_quality_reason": source_quality_for_url(url).reason,
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
                "source_quality": source_quality_for_url(url).label,
                "source_quality_score": source_quality_for_url(url).score,
                "source_quality_reason": source_quality_for_url(url).reason,
                "metadata": {"sourceURL": url},
            }));
        }
    }
    rows
}

fn parallel_extract_response(url: &str, documents: Vec<Value>) -> Result<String, ToolError> {
    let first_content = documents
        .first()
        .and_then(|d| d.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let source_summary = source_quality_summary(&documents);
    let response = json!({
        "url": url,
        "content": first_content,
        "results": documents,
        "provider": "parallel",
        "source_quality_summary": source_summary,
    });
    serde_json::to_string_pretty(&response)
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize result: {e}")))
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
/// 8. Local fallback helper response
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
        "fallback"
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
/// 4. Parallel REST when `PARALLEL_API_KEY` is configured
/// 5. Simple local extractor fallback
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
    } else if env_present_nonempty("PARALLEL_API_KEY") {
        "parallel"
    } else {
        "simple"
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
                let rows: Vec<Value> = citations
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(|url| !url.trim().is_empty())
                    .enumerate()
                    .map(|(idx, url)| search_result("", url, "", None, idx + 1))
                    .collect();
                ranked_search_results(rows)
                    .into_iter()
                    .take(limit)
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

        let formatted = Self::parse_results(&data, limit);
        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "provider": "xai",
            "model": &self.model,
            "source_quality_summary": source_summary,
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
        for row in results {
            let Some(url) = row.get("url").and_then(|v| v.as_str()).map(str::trim) else {
                continue;
            };
            if url.is_empty() {
                continue;
            }
            normalized.push(search_result(
                row.get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim(),
                url,
                row.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim(),
                None,
                normalized.len() + 1,
            ));
        }
        if !normalized.is_empty() {
            return ranked_search_results(normalized)
                .into_iter()
                .take(limit)
                .collect();
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
        results.push(search_result("", url, description, None, results.len() + 1));
        if results.len() >= limit {
            break;
        }
    }
    ranked_search_results(results)
        .into_iter()
        .take(limit)
        .collect()
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
        let source_summary = source_quality_summary(&formatted);
        serde_json::to_string_pretty(&json!({
            "results": formatted,
            "transport": self.transport.label(),
            "source_quality_summary": source_summary,
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

    let rows: Vec<Value> = candidates
        .into_iter()
        .flatten()
        .find_map(|v| v.as_array())
        .map(|results| {
            results
                .iter()
                .enumerate()
                .map(|(idx, result)| {
                    let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let text = result
                        .get("description")
                        .or_else(|| result.get("content"))
                        .or_else(|| result.get("markdown"))
                        .or_else(|| result.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    search_result(
                        title,
                        url,
                        text,
                        result.get("score").and_then(|v| v.as_f64()),
                        idx + 1,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    ranked_search_results(rows)
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
