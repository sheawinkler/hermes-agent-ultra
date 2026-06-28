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
                "DDG_SEARCH_TIMEOUT_SECONDS",
                "HERMES_DDGS_TIMEOUT_SECONDS",
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
    fn source_quality_classifies_primary_community_and_secondary_urls() {
        assert_eq!(
            source_quality_for_url("https://github.com/NousResearch/hermes-agent").label,
            "primary"
        );
        assert_eq!(
            source_quality_for_url("https://www.reddit.com/r/solana/comments/example").label,
            "community"
        );
        assert_eq!(
            source_quality_for_url("https://medium.com/example/post").label,
            "secondary"
        );
    }

    #[test]
    fn ranked_search_results_prefers_primary_sources_before_provider_score() {
        let rows = vec![
            search_result(
                "SEO summary",
                "https://medium.com/example/post",
                "summary",
                Some(0.99),
                1,
            ),
            search_result(
                "Official docs",
                "https://docs.solana.com/developing",
                "docs",
                Some(0.10),
                2,
            ),
        ];

        let ranked = ranked_search_results(rows);
        assert_eq!(ranked[0]["title"], "Official docs");
        assert_eq!(ranked[0]["source_quality"], "primary");
        assert_eq!(ranked[0]["original_position"], 2);
        assert_eq!(ranked[0]["position"], 1);
        assert_eq!(ranked[0]["source_rank"], 1);
        assert_eq!(
            source_quality_summary(&ranked),
            json!({"primary": 1, "community": 0, "secondary": 1})
        );
    }

    #[test]
    fn search_backend_choice_uses_xai_only_when_explicit() {
        let _scope = EnvScope::new();
        std::env::set_var("XAI_API_KEY", "xai-key");
        assert_eq!(search_backend_choice_from_env(), "fallback");
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "xai");
        assert_eq!(search_backend_choice_from_env(), "xai");
    }

    #[test]
    fn xai_from_env_defaults_to_grok_build_model() {
        let _scope = EnvScope::new();
        std::env::set_var("XAI_API_KEY", "xai-key");
        let backend = XaiWebSearchBackend::from_env().expect("xai backend from env");
        assert_eq!(backend.model(), "grok-build-0.1");
        assert_eq!(backend.base_url(), XAI_BASE_URL_DEFAULT);
    }

    #[test]
    fn xai_from_env_honors_model_and_base_url_overrides() {
        let _scope = EnvScope::new();
        std::env::set_var("XAI_API_KEY", "xai-key");
        std::env::set_var("XAI_BASE_URL", "https://proxy.example.com/xai/");
        std::env::set_var("HERMES_WEB_XAI_MODEL", "grok-custom");
        let backend = XaiWebSearchBackend::from_env().expect("xai backend from env");
        assert_eq!(backend.model(), "grok-custom");
        assert_eq!(backend.base_url(), "https://proxy.example.com/xai");
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
    fn search_backend_choice_uses_fallback_as_last_resort() {
        let _scope = EnvScope::new();
        assert_eq!(search_backend_choice_from_env(), "fallback");
    }

    #[test]
    fn search_backend_choice_accepts_explicit_ddg() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "ddgs");
        assert_eq!(search_backend_choice_from_env(), "ddgs");
    }

    #[test]
    fn duckduckgo_from_env_applies_bounded_timeout() {
        let _scope = EnvScope::new();
        std::env::set_var("DDG_SEARCH_TIMEOUT_SECONDS", "0.25");
        let backend = DuckDuckGoSearchBackend::from_env().expect("ddg backend");
        assert_eq!(backend.timeout(), std::time::Duration::from_millis(250));

        std::env::set_var("DDG_SEARCH_TIMEOUT_SECONDS", "0");
        let backend = DuckDuckGoSearchBackend::from_env().expect("ddg backend");
        assert_eq!(
            backend.timeout(),
            std::time::Duration::from_secs(DDG_SEARCH_TIMEOUT_SECS_DEFAULT)
        );
    }

    #[tokio::test]
    async fn duckduckgo_search_times_out_slow_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(200))
                    .set_body_json(json!({"RelatedTopics": []})),
            )
            .mount(&server)
            .await;

        let backend = DuckDuckGoSearchBackend::with_timeout(
            server.uri(),
            std::time::Duration::from_millis(50),
        );
        let err = backend
            .search("slow query", 3, None)
            .await
            .expect_err("slow ddg response should time out");
        assert!(err.to_string().contains("DuckDuckGo search request failed"));
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
        assert_eq!(extract_backend_choice_from_env(), "simple");
        std::env::set_var("TAVILY_API_KEY", "tavily-key");
        assert_eq!(extract_backend_choice_from_env(), "tavily");
        std::env::set_var("FIRECRAWL_API_KEY", "fire-key");
        assert_eq!(extract_backend_choice_from_env(), "firecrawl");
    }

    #[test]
    fn extract_backend_choice_uses_parallel_key_when_present() {
        let _scope = EnvScope::new();
        std::env::set_var("PARALLEL_API_KEY", "parallel-key");
        assert_eq!(extract_backend_choice_from_env(), "parallel");
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
        assert_eq!(docs[0]["source_quality"], "secondary");
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
    async fn parallel_without_key_returns_configuration_error() {
        let backend = ParallelWebBackend::with_endpoints(
            None,
            PARALLEL_BASE_URL_DEFAULT.to_string(),
            "advanced".to_string(),
        );
        let err = backend
            .search("rust async", 5, None)
            .await
            .expect_err("parallel search requires an API key");
        assert!(err.to_string().contains("PARALLEL_API_KEY"));
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
