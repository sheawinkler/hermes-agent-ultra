use super::*;

fn codex_jwt_with_account(account_id: Option<&str>) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = match account_id {
        Some(account_id) => serde_json::json!({
            "sub": "user-xyz",
            "exp": 9_999_999_999_i64,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_plan_type": "plus"
            }
        }),
        None => serde_json::json!({
            "sub": "user-xyz",
            "exp": 9_999_999_999_i64
        }),
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    format!("{header}.{payload}.sig")
}

fn header_value<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

#[test]
fn codex_cloudflare_headers_match_codex_cli_rs_contract() {
    let token = codex_jwt_with_account(Some("acct-abc-999"));
    let headers = codex_cloudflare_headers(Some(token.as_str()));

    assert_eq!(header_value(&headers, "originator"), Some("codex_cli_rs"));
    assert!(header_value(&headers, "User-Agent")
        .expect("user agent")
        .starts_with("codex_cli_rs/"));
    assert_eq!(
        header_value(&headers, "ChatGPT-Account-ID"),
        Some("acct-abc-999")
    );
    assert!(header_value(&headers, "chatgpt-account-id").is_none());
    assert!(header_value(&headers, "ChatGPT-Account-Id").is_none());
}

#[test]
fn codex_cloudflare_headers_ignore_malformed_or_missing_account_tokens() {
    for token in ["not-a-jwt", "", "only.one", "  ", "...."] {
        let headers = codex_cloudflare_headers(Some(token));
        assert_eq!(header_value(&headers, "originator"), Some("codex_cli_rs"));
        assert!(header_value(&headers, "ChatGPT-Account-ID").is_none());
    }

    let token = codex_jwt_with_account(None);
    let headers = codex_cloudflare_headers(Some(token.as_str()));
    assert_eq!(header_value(&headers, "originator"), Some("codex_cli_rs"));
    assert!(header_value(&headers, "ChatGPT-Account-ID").is_none());
}

#[test]
fn openai_codex_provider_attaches_cloudflare_headers_to_requests() {
    let token = codex_jwt_with_account(Some("acct-request"));
    let provider = openai_codex_provider(token.as_str(), "gpt-5.4", None);
    assert_eq!(provider.inner.base_url, OPENAI_CODEX_BASE_URL);

    let request = provider
        .inner
        .build_request(
            &Client::new(),
            &format!("{}/chat/completions", OPENAI_CODEX_BASE_URL),
            token.as_str(),
            &serde_json::json!({"model": "gpt-5.4", "messages": []}),
        )
        .build()
        .expect("request");
    let headers = request.headers();

    assert_eq!(headers.get("originator").unwrap(), "codex_cli_rs");
    assert!(headers
        .get("User-Agent")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("codex_cli_rs/"));
    assert_eq!(headers.get("ChatGPT-Account-ID").unwrap(), "acct-request");
}

#[test]
fn chatgpt_oauth_dynamic_alias_resolves_to_supported_wire_model() {
    let token = codex_jwt_with_account(Some("acct-dynamic"));

    assert_eq!(
        resolve_openai_chatgpt_dynamic_wire_model("dynamic", token.as_str()),
        OPENAI_CODEX_DYNAMIC_WIRE_MODEL
    );
    assert_eq!(
        resolve_openai_chatgpt_dynamic_wire_model("openai:dynamic", token.as_str()),
        OPENAI_CODEX_DYNAMIC_WIRE_MODEL
    );
    assert_eq!(
        resolve_openai_chatgpt_dynamic_wire_model("gpt-5.5", token.as_str()),
        "gpt-5.5"
    );
    assert_eq!(
        resolve_openai_chatgpt_dynamic_wire_model("dynamic", "sk-standard-api-key"),
        "dynamic"
    );
    assert_eq!(
        resolve_openai_chatgpt_dynamic_wire_model("dynamic", "not-a-jwt"),
        "dynamic"
    );
    assert_eq!(
        resolve_openai_compatible_dynamic_wire_model(
            "dynamic",
            "not-a-jwt",
            "https://api.openai.com/v1"
        ),
        OPENAI_CODEX_DYNAMIC_WIRE_MODEL
    );
    assert_eq!(
        resolve_openai_compatible_dynamic_wire_model(
            "dynamic",
            "not-a-jwt",
            "https://example.test/v1"
        ),
        "dynamic"
    );
}

#[test]
fn openai_compatible_extra_body_cannot_restore_dynamic_wire_model() {
    let provider = GenericProvider::new(
        "https://api.openai.com/v1",
        "sk-test",
        OPENAI_CODEX_DYNAMIC_WIRE_MODEL,
    );
    let extra_body = serde_json::json!({
        "model": "dynamic",
        "service_tier": "priority"
    });

    let body = provider.chat_request_body(ChatRequestParams {
        messages: &[Message::user("Say ok")],
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: OPENAI_CODEX_DYNAMIC_WIRE_MODEL,
        extra_body: Some(&extra_body),
        stream: false,
    });

    assert_eq!(body["model"], OPENAI_CODEX_DYNAMIC_WIRE_MODEL);
    assert_eq!(body["service_tier"], "priority");
}

#[test]
fn generic_provider_omits_authorization_for_local_no_key_marker() {
    let provider = GenericProvider::new("http://127.0.0.1:8080/v1", "local-no-key", "local-gguf")
        .with_provider_profile("llama-cpp");
    let request = provider
        .build_request(
            &Client::new(),
            "http://127.0.0.1:8080/v1/chat/completions",
            "local-no-key",
            &serde_json::json!({"model": "local-gguf", "messages": []}),
        )
        .build()
        .expect("request");
    assert!(request.headers().get("Authorization").is_none());
}

#[test]
fn openai_codex_provider_skips_cloudflare_headers_for_non_chatgpt_override() {
    let token = codex_jwt_with_account(Some("acct-request"));
    let provider = openai_codex_provider(
        token.as_str(),
        "gpt-5.4",
        Some("https://openrouter.ai/api/v1"),
    );

    let request = provider
        .inner
        .build_request(
            &Client::new(),
            "https://openrouter.ai/api/v1/chat/completions",
            token.as_str(),
            &serde_json::json!({"model": "gpt-5.4", "messages": []}),
        )
        .build()
        .expect("request");

    assert!(request.headers().get("originator").is_none());
    assert!(request.headers().get("ChatGPT-Account-ID").is_none());
}

#[test]
fn generic_provider_attaches_kimi_code_user_agent_for_code_endpoint() {
    let provider = GenericProvider::new(
        provider_profiles::KIMI_CODE_BASE_URL,
        "sk-kimi-test",
        "kimi-k2.6",
    );
    let request = provider
        .build_request(
            &Client::new(),
            &format!("{}/chat/completions", provider_profiles::KIMI_CODE_BASE_URL),
            "sk-kimi-test",
            &serde_json::json!({"model": "kimi-k2.6", "messages": []}),
        )
        .build()
        .expect("request");

    assert_eq!(
        request
            .headers()
            .get("User-Agent")
            .and_then(|h| h.to_str().ok()),
        Some(provider_profiles::KIMI_CODE_USER_AGENT)
    );
}

#[test]
fn generic_provider_does_not_attach_kimi_code_user_agent_to_legacy_endpoint() {
    let provider = GenericProvider::new(
        provider_profiles::KIMI_LEGACY_BASE_URL,
        "sk-legacy",
        "kimi-k2.6",
    );
    let request = provider
        .build_request(
            &Client::new(),
            &format!(
                "{}/chat/completions",
                provider_profiles::KIMI_LEGACY_BASE_URL
            ),
            "sk-legacy",
            &serde_json::json!({"model": "kimi-k2.6", "messages": []}),
        )
        .build()
        .expect("request");

    assert!(request.headers().get("User-Agent").is_none());
}

#[test]
fn generic_provider_captures_nous_credits_headers() {
    hermes_core::credits::clear_last_nous_credits_state();
    let provider = GenericProvider::new(
        "https://inference-api.nousresearch.com/v1",
        "nous-key",
        "openai/gpt-5.5-pro",
    );
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in [
        ("x-nous-credits-version", "1"),
        ("x-nous-credits-remaining-micros", "12000000"),
        ("x-nous-credits-remaining-usd", "12.00"),
        ("x-nous-credits-subscription-micros", "5000000"),
        ("x-nous-credits-subscription-usd", "5.00"),
        ("x-nous-credits-subscription-limit-micros", "10000000"),
        ("x-nous-credits-subscription-limit-usd", "10.00"),
        ("x-nous-credits-rollover-micros", "0"),
        ("x-nous-credits-purchased-micros", "7000000"),
        ("x-nous-credits-purchased-usd", "7.00"),
        ("x-nous-credits-denominator-kind", "subscription_cap"),
        ("x-nous-credits-paid-access", "true"),
    ] {
        headers.insert(
            reqwest::header::HeaderName::from_static(key),
            reqwest::header::HeaderValue::from_static(value),
        );
    }

    provider.capture_nous_credits_headers(&headers);
    let state = hermes_core::credits::last_nous_credits_state().expect("captured state");
    assert_eq!(state.remaining_usd, "12.00");
    assert_eq!(state.used_fraction(), Some(0.5));
    hermes_core::credits::clear_last_nous_credits_state();
}

#[test]
fn generic_provider_request_timeout_survives_client_rebuilds() {
    let provider = GenericProvider::new("https://api.example.com/v1", "sk-test", "model")
        .with_optional_request_timeout_seconds(Some(45.5));

    assert_eq!(
        provider.configured_request_timeout(),
        Some(Duration::from_secs_f64(45.5))
    );

    provider.refresh_client("unit test");
    assert_eq!(
        provider.configured_request_timeout(),
        Some(Duration::from_secs_f64(45.5))
    );
}

#[test]
fn generic_provider_ignores_invalid_request_timeout_seconds() {
    for value in [
        None,
        Some(0.0),
        Some(-1.0),
        Some(f64::INFINITY),
        Some(f64::NAN),
    ] {
        let provider = GenericProvider::new("https://api.example.com/v1", "sk-test", "model")
            .with_optional_request_timeout_seconds(value);
        assert_eq!(provider.configured_request_timeout(), None);
    }
}

#[test]
fn openai_codex_provider_applies_request_timeout_seconds() {
    let provider = openai_codex_provider_with_timeout("sk-test", "gpt-5.4", None, Some(60.0));

    assert_eq!(
        provider.inner.configured_request_timeout(),
        Some(Duration::from_secs(60))
    );
}

#[test]
fn test_format_tools_for_openai_api_shape() {
    let tools = vec![ToolSchema::new(
        "read_file",
        "Read file content",
        hermes_core::JsonSchema::new("object"),
    )];
    let formatted = GenericProvider::format_tools_for_openai_api(&tools);
    let rows = formatted.as_array().expect("tools array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["type"], "function");
    assert_eq!(rows[0]["function"]["name"], "read_file");
    assert_eq!(rows[0]["function"]["description"], "Read file content");
    assert_eq!(rows[0]["function"]["parameters"]["type"], "object");
}

#[test]
fn test_moonshot_tool_schema_sanitizer_repairs_mcp_shapes() {
    let params = serde_json::json!({
        "type": "object",
        "properties": {
            "query": {"description": "search text"},
            "filter": {
                "type": "string",
                "anyOf": [
                    {"type": "string"},
                    {"type": "null"}
                ]
            },
            "tags": {
                "type": "array",
                "items": {"description": "tag"}
            },
            "db_type": {
                "anyOf": [
                    {"enum": ["mysql", "postgresql", "", null]},
                    {"type": "null"}
                ],
                "nullable": true
            },
            "payload": {"$ref": "#/$defs/Payload"}
        },
        "$defs": {"Payload": {"type": "object", "properties": {}}}
    });

    let out = sanitize_moonshot_tool_parameters(&params);
    assert_eq!(out["type"], "object");
    assert_eq!(out["properties"]["query"]["type"], "string");
    assert_eq!(out["properties"]["filter"]["type"], "string");
    assert!(out["properties"]["filter"].get("anyOf").is_none());
    assert_eq!(out["properties"]["tags"]["items"]["type"], "string");
    assert_eq!(out["properties"]["db_type"]["type"], "string");
    assert_eq!(
        out["properties"]["db_type"]["enum"],
        serde_json::json!(["mysql", "postgresql"])
    );
    assert!(out["properties"]["db_type"].get("nullable").is_none());
    assert!(out["properties"]["payload"].get("type").is_none());
    assert_eq!(out["properties"]["payload"]["$ref"], "#/$defs/Payload");
}

#[test]
fn test_moonshot_model_tool_formatter_applies_sanitizer() {
    let mut params = hermes_core::JsonSchema::new("object");
    params.properties = Some(Default::default());
    params
        .properties
        .as_mut()
        .expect("properties")
        .insert("q".to_string(), serde_json::json!({"description": "query"}));
    let tools = vec![ToolSchema::new("search", "Search", params)];

    let formatted = format_tools_for_openai_api_with_model(
        &tools,
        "openrouter/moonshotai/kimi-k2.6",
        "https://openrouter.ai/api/v1",
    );
    assert_eq!(
        formatted[0]["function"]["parameters"]["properties"]["q"]["type"],
        "string"
    );
    assert!(is_moonshot_model("nous/moonshotai/kimi-k2-thinking"));
    assert!(!is_moonshot_model("anthropic/claude-sonnet-4.6"));
}

#[test]
fn test_merge_extra_body_fields_strips_local_request_controls() {
    let extra = serde_json::json!({
        "strict_api": true,
        "strict_tool_calls": true,
        "provider_strict": true,
        "provider_profile": "openrouter",
        "provider_preferences": {"allow": ["anthropic"]},
        "supports_vision": true,
        "temperature": 0.2
    });
    let mut body = serde_json::json!({"model": "m", "messages": []});
    GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
    assert!(body.get("strict_api").is_none());
    assert!(body.get("strict_tool_calls").is_none());
    assert!(body.get("provider_strict").is_none());
    assert!(body.get("provider_profile").is_none());
    assert!(body.get("provider_preferences").is_none());
    assert!(body.get("supports_vision").is_none());
    assert_eq!(body["temperature"], 0.2);
}

#[test]
fn test_native_gemini_request_body_strips_openai_extra_body_fields() {
    let provider = GenericProvider::new(
        provider_profiles::GEMINI_NATIVE_BASE_URL,
        "key",
        "gemini-2.5-pro",
    )
    .with_provider_profile("nous");
    let messages = vec![Message::user("hello")];
    let extra = serde_json::json!({
        "provider_profile": "nous",
        "tags": ["bad-openai-field"],
        "reasoning_effort": "high",
        "service_tier": "flex",
        "supports_vision": true,
        "thinking_config": {"thinking_budget": 1024},
        "thinkingConfig": {"includeThoughts": true}
    });

    let body = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "gemini-2.5-pro",
        extra_body: Some(&extra),
        stream: false,
    });

    assert_eq!(
        body["thinking_config"],
        serde_json::json!({"thinking_budget": 1024})
    );
    assert_eq!(
        body["thinkingConfig"],
        serde_json::json!({"includeThoughts": true})
    );
    assert!(body.get("tags").is_none());
    assert!(body.get("reasoning").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("service_tier").is_none());
    assert!(body.get("provider_profile").is_none());
    assert!(body.get("supports_vision").is_none());
}

#[test]
fn test_provider_profile_request_body_defaults_and_qwen_messages() {
    let provider = GenericProvider::new("https://dashscope.example/v1", "key", "qwen3.5")
        .with_provider_profile("qwen-oauth");
    let messages = vec![Message::system("Be helpful"), Message::user("hello")];
    let extra = serde_json::json!({
        "qwen_session_metadata": {"sessionId": "s123", "promptId": "p456"},
        "provider_profile": "qwen-oauth"
    });

    let body = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: Some(0.4),
        effective_model: "qwen3.5",
        extra_body: Some(&extra),
        stream: false,
    });

    assert_eq!(body["max_tokens"], 65_536);
    assert_eq!(body["temperature"], 0.4);
    assert_eq!(body["vl_high_resolution_images"], true);
    assert_eq!(
        body["metadata"],
        serde_json::json!({"sessionId": "s123", "promptId": "p456"})
    );
    assert!(body.get("qwen_session_metadata").is_none());
    assert!(body.get("provider_profile").is_none());
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        serde_json::json!({"type": "ephemeral"})
    );
    assert_eq!(
        body["messages"][1]["content"][0],
        serde_json::json!({"type": "text", "text": "hello"})
    );
}

#[test]
fn test_provider_profile_request_body_kimi_reasoning_contract() {
    let provider = GenericProvider::new("https://api.moonshot.ai/v1", "key", "kimi-k2")
        .with_provider_profile("kimi");
    let messages = vec![Message::user("hello")];

    let enabled = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: Some(0.7),
        effective_model: "kimi-k2",
        extra_body: Some(&serde_json::json!({"reasoning": {"enabled": true, "effort": "high"}})),
        stream: false,
    });
    assert_eq!(enabled["max_tokens"], 32_000);
    assert!(enabled.get("temperature").is_none());
    assert_eq!(enabled["thinking"], serde_json::json!({"type": "enabled"}));
    assert_eq!(enabled["reasoning_effort"], "high");
    assert!(enabled.get("reasoning").is_none());

    let disabled = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: Some(0.7),
        effective_model: "kimi-k2",
        extra_body: Some(&serde_json::json!({"reasoning_config": {"enabled": false}})),
        stream: false,
    });
    assert_eq!(
        disabled["thinking"],
        serde_json::json!({"type": "disabled"})
    );
    assert!(disabled.get("reasoning_effort").is_none());
}

#[test]
fn test_provider_profile_request_body_openrouter_nous_and_custom_contracts() {
    let messages = vec![Message::user("hello")];

    let openrouter = GenericProvider::new(
        "https://openrouter.ai/api/v1",
        "key",
        "openrouter/pareto-code",
    )
    .with_provider_profile("openrouter");
    let or_body = openrouter.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "openrouter/pareto-code",
        extra_body: Some(&serde_json::json!({
            "provider_preferences": {"allow": ["anthropic"], "sort": "price"},
            "openrouter_min_coding_score": 0.65,
            "supports_reasoning": true,
            "session_id": "sess-123"
        })),
        stream: false,
    });
    assert_eq!(
        or_body["provider"],
        serde_json::json!({"allow": ["anthropic"], "sort": "price"})
    );
    assert_eq!(or_body["session_id"], "sess-123");
    assert_eq!(
        or_body["plugins"],
        serde_json::json!([{"id": "pareto-router", "min_coding_score": 0.65}])
    );
    assert_eq!(
        or_body["reasoning"],
        serde_json::json!({"enabled": true, "effort": "medium"})
    );
    assert!(or_body.get("provider_preferences").is_none());

    let anthropic_mandatory = openrouter.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "anthropic/claude-sonnet-4.6",
        extra_body: Some(&serde_json::json!({
            "supports_reasoning": true,
            "reasoning": {"enabled": true, "effort": "high"}
        })),
        stream: false,
    });
    assert!(anthropic_mandatory.get("reasoning").is_none());
    assert_eq!(anthropic_mandatory["verbosity"], "high");

    let anthropic_disabled = openrouter.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "anthropic/claude-sonnet-4.6",
        extra_body: Some(&serde_json::json!({
            "supports_reasoning": true,
            "reasoning": {"enabled": false}
        })),
        stream: false,
    });
    assert!(anthropic_disabled.get("reasoning").is_none());
    assert!(anthropic_disabled.get("verbosity").is_none());

    let nous = GenericProvider::new(
        "https://inference-api.nousresearch.com/v1",
        "key",
        "hermes-3",
    )
    .with_provider_profile("nous");
    let nous_body = nous.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "hermes-3",
        extra_body: Some(&serde_json::json!({
            "supports_reasoning": true,
            "reasoning": {"enabled": false}
        })),
        stream: false,
    });
    assert_eq!(
        nous_body["tags"],
        serde_json::json!([
            provider_profiles::NOUS_PRODUCT_TAG,
            provider_profiles::hermes_client_tag()
        ])
    );
    assert!(nous_body.get("reasoning").is_none());

    let custom = GenericProvider::new("http://127.0.0.1:11434/v1", "key", "qwen3:72b")
        .with_provider_profile("ollama-local");
    let custom_body = custom.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "qwen3:72b",
        extra_body: Some(&serde_json::json!({"ollama_num_ctx": 131072})),
        stream: false,
    });
    assert_eq!(custom_body["max_tokens"], 65_536);
    assert_eq!(custom_body["options"]["num_ctx"], 131_072);
    assert!(custom_body.get("ollama_num_ctx").is_none());
}

#[test]
fn test_provider_profile_request_body_minimax_m3_openai_reasoning_contract() {
    let messages = vec![Message::user("hello")];
    let provider = GenericProvider::new("https://api.minimax.io/v1", "key", "MiniMax-M3")
        .with_provider_profile("minimax");

    let enabled = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "MiniMax-M3",
        extra_body: Some(&serde_json::json!({"reasoning": {"effort": "high"}})),
        stream: false,
    });
    assert_eq!(enabled["reasoning_split"], true);
    assert_eq!(enabled["thinking"], serde_json::json!({"type": "adaptive"}));
    assert!(enabled.get("reasoning").is_none());

    let disabled = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "minimax/minimax-m3",
        extra_body: Some(&serde_json::json!({"reasoning_config": {"enabled": false}})),
        stream: false,
    });
    assert_eq!(
        disabled["thinking"],
        serde_json::json!({"type": "disabled"})
    );

    let anthropic_route =
        GenericProvider::new("https://api.minimax.io/anthropic", "key", "MiniMax-M3")
            .with_provider_profile("minimax");
    let body = anthropic_route.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "MiniMax-M3",
        extra_body: Some(&serde_json::json!({"reasoning": {"effort": "high"}})),
        stream: false,
    });
    assert!(body.get("reasoning_split").is_none());
    assert!(body.get("thinking").is_none());
}

#[test]
fn test_opencode_go_kimi_reasoning_uses_moonshot_shape() {
    let provider = GenericProvider::new("https://opencode.ai/zen/go/v1", "test-key", "kimi-k2.6");
    let extra = serde_json::json!({"reasoning": {"effort": "xhigh"}});
    let mut body = serde_json::json!({"model": "kimi-k2.6", "messages": []});

    GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
    provider.apply_opencode_go_reasoning_controls(&mut body, "moonshotai/kimi-k2.6");

    assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
    assert_eq!(body["reasoning_effort"], "high");
    assert!(body.get("reasoning").is_none());
}

#[test]
fn test_opencode_go_deepseek_reasoning_uses_thinking_shape() {
    let provider = GenericProvider::new(
        "https://opencode.ai/zen/go/v1",
        "test-key",
        "deepseek-v4-pro",
    );
    let extra = serde_json::json!({"reasoning": {"effort": "max"}});
    let mut body = serde_json::json!({"model": "deepseek-v4-pro", "messages": []});

    GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
    provider.apply_opencode_go_reasoning_controls(&mut body, "deepseek/deepseek-v4-pro");

    assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
    assert_eq!(body["reasoning_effort"], "max");
    assert!(body.get("reasoning").is_none());
}

#[test]
fn test_opencode_go_non_target_model_drops_reasoning_controls() {
    let provider = GenericProvider::new("https://opencode.ai/zen/go/v1", "test-key", "glm-5.1");
    let extra = serde_json::json!({"reasoning": {"effort": "high"}});
    let mut body = serde_json::json!({"model": "glm-5.1", "messages": []});

    GenericProvider::merge_extra_body_fields(&mut body, Some(&extra));
    provider.apply_opencode_go_reasoning_controls(&mut body, "glm-5.1");

    assert!(body.get("thinking").is_none());
    assert!(body.get("reasoning").is_none());
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn test_opencode_go_model_detection_handles_prefixes_and_variants() {
    assert_eq!(flat_model_name("opencode-go:kimi-k2.6"), "kimi-k2.6");
    assert_eq!(flat_model_name("opencode-go:kimi-k2.6:fast"), "kimi-k2.6");
    assert_eq!(flat_model_name("moonshotai/kimi-k2.6:fast"), "kimi-k2.6");
    assert_eq!(
        flat_model_name("openrouter:deepseek/deepseek-reasoner:max"),
        "deepseek-reasoner"
    );
}

#[test]
fn test_sanitize_messages_for_strict_api_reconstructs_flattened_tool_call_function() {
    let messages = vec![Message::assistant_with_tool_calls(
        None,
        vec![ToolCall {
            id: "call_123".to_string(),
            function: FunctionCall {
                name: "skills_list".to_string(),
                arguments: "{\"category\":\"builtin\"}".to_string(),
            },
            extra_content: None,
        }],
    )];

    let sanitized =
        GenericProvider::sanitize_messages_for_api(&messages, true, "gpt-4o", None, None);
    let tc = &sanitized[0]["tool_calls"][0];
    assert_eq!(tc["id"], "call_123");
    assert_eq!(tc["type"], "function");
    assert_eq!(tc["function"]["name"], "skills_list");
    assert_eq!(tc["function"]["arguments"], "{\"category\":\"builtin\"}");
    assert!(tc.get("name").is_none());
    assert!(tc.get("arguments").is_none());
}

#[test]
fn test_sanitize_messages_for_strict_api_disabled_preserves_flattened_shape() {
    let messages = vec![Message::assistant_with_tool_calls(
        None,
        vec![ToolCall {
            id: "call_abc".to_string(),
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: "{\"path\":\"a.txt\"}".to_string(),
            },
            extra_content: None,
        }],
    )];
    let sanitized =
        GenericProvider::sanitize_messages_for_api(&messages, false, "gpt-4o", None, None);
    let tc = &sanitized[0]["tool_calls"][0];
    assert_eq!(tc["name"], "read_file");
    assert_eq!(tc["arguments"], "{\"path\":\"a.txt\"}");
    assert!(tc.get("function").is_none());
}

#[test]
fn test_nous_profile_strict_controls_sanitize_tool_replay_without_leaking_controls() {
    let provider = GenericProvider::new(
        "https://inference-api.nousresearch.com/v1",
        "key",
        "openai/gpt-5.5",
    )
    .with_provider_profile("nous");
    let messages = vec![
        Message::assistant_with_tool_calls(
            None,
            vec![ToolCall {
                id: "call_read".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"a.txt\"}".to_string(),
                },
                extra_content: None,
            }],
        ),
        Message::tool_result_with_name("call_read", "read_file", "{\"result\":\"ok\"}"),
    ];

    let body = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "openai/gpt-5.5",
        extra_body: Some(&serde_json::json!({
            "strict_api": true,
            "provider_strict": true
        })),
        stream: false,
    });

    assert!(body.get("strict_api").is_none());
    assert!(body.get("provider_strict").is_none());
    let tc = &body["messages"][0]["tool_calls"][0];
    assert_eq!(tc["id"], "call_read");
    assert_eq!(tc["type"], "function");
    assert_eq!(tc["function"]["name"], "read_file");
    assert_eq!(tc["function"]["arguments"], "{\"path\":\"a.txt\"}");
    assert!(tc.get("name").is_none());
    assert!(tc.get("arguments").is_none());
    assert_eq!(body["messages"][1]["role"], "tool");
    assert_eq!(body["messages"][1]["tool_call_id"], "call_read");
}

#[test]
fn test_sanitize_messages_for_api_decodes_acp_multimodal_user_parts_for_vision_models() {
    let parts = serde_json::json!([
        {"type": "text", "text": "inspect"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
    ]);
    let marker = format!("{}{}", ACP_MULTIMODAL_PREFIX, parts);
    let messages = vec![Message::user(marker)];
    let sanitized =
        GenericProvider::sanitize_messages_for_api(&messages, false, "gpt-4o", None, None);
    assert!(sanitized[0]["content"].is_array());
    assert_eq!(sanitized[0]["content"][1]["type"], "image_url");
}

#[test]
fn test_sanitize_messages_for_api_collapses_images_for_non_vision_models() {
    let parts = serde_json::json!([
        {"type": "text", "text": "inspect"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
    ]);
    let marker = format!("{}{}", ACP_MULTIMODAL_PREFIX, parts);
    let messages = vec![Message::user(marker)];
    let sanitized =
        GenericProvider::sanitize_messages_for_api(&messages, false, "deepseek-chat", None, None);
    let content = sanitized[0]["content"].as_str().expect("collapsed text");
    assert!(content.contains("inspect"));
    assert!(content.contains("[Attached image]"));
    assert!(!content.contains("image_url"));
}

#[test]
fn test_provider_profile_vision_preserves_acp_multimodal_parts() {
    let parts = serde_json::json!([
        {"type": "text", "text": "inspect"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
    ]);
    let messages = vec![Message::user(format!("{ACP_MULTIMODAL_PREFIX}{parts}"))];
    let provider = GenericProvider::new("https://api.xiaomimimo.com/v1", "key", "mimo-v2-omni")
        .with_provider_profile("mimo");

    let body = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "mimo-v2-omni",
        extra_body: None,
        stream: false,
    });

    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
}

#[test]
fn test_supports_vision_override_can_disable_multimodal_parts() {
    let parts = serde_json::json!([
        {"type": "text", "text": "inspect"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
    ]);
    let messages = vec![Message::user(format!("{ACP_MULTIMODAL_PREFIX}{parts}"))];
    let provider = GenericProvider::new("https://api.openai.com/v1", "key", "gpt-4o");
    let extra = serde_json::json!({"supports_vision": false});

    let body = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "gpt-4o",
        extra_body: Some(&extra),
        stream: false,
    });

    let content = body["messages"][0]["content"]
        .as_str()
        .expect("collapsed text");
    assert!(content.contains("[Attached image]"));
    assert!(body.get("supports_vision").is_none());
}

#[test]
fn test_xiaomi_profile_flattens_multimodal_tool_messages_only() {
    let parts = serde_json::json!([
        {"type": "text", "text": "tool summary"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
    ]);
    let messages = vec![
        Message::user(format!("{ACP_MULTIMODAL_PREFIX}{parts}")),
        Message::tool_result("tool-call-1", format!("{ACP_MULTIMODAL_PREFIX}{parts}")),
    ];
    let provider = GenericProvider::new("https://api.xiaomimimo.com/v1", "key", "mimo-v2-omni")
        .with_provider_profile("xiaomi");

    let body = provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: "mimo-v2-omni",
        extra_body: None,
        stream: false,
    });

    assert!(body["messages"][0]["content"].is_array());
    assert!(body["messages"][1]["content"].is_string());
    assert!(body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("[Attached image]"));
}

#[test]
fn test_parse_openai_response_basic() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Hello!"
            },
            "finish_reason": "stop"
        }],
        "model": "gpt-4o",
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });
    let resp = parse_openai_response(&json).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("Hello!"));
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 15);
}

#[test]
fn test_parse_openai_response_null_content_is_safe() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_null_content",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"Cargo.toml\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "reasoning-tool-only"
    });

    let resp = parse_openai_response(&json).expect("null content response should parse");

    assert_eq!(resp.message.content.as_deref(), Some(""));
    let calls = resp.message.tool_calls.as_ref().expect("tool calls");
    assert_eq!(calls[0].id, "call_null_content");
    assert_eq!(calls[0].function.name, "read_file");
}

#[test]
fn test_parse_openai_response_no_choices_includes_provider_context() {
    let json = serde_json::json!({
        "status": 400,
        "message": "This request is not valid. Check the model name and other parameters. Additional info: Provider returned error",
    });
    let err = parse_openai_response(&json).unwrap_err().to_string();
    assert!(err.contains("No choices in response"));
    assert!(err.contains("status=400"));
    assert!(err.contains("Provider returned error"));
}

#[test]
fn test_parse_openai_response_empty_choices_includes_error_context() {
    let json = serde_json::json!({
        "choices": [],
        "error": {"message": "Check that you're sending a valid payload."},
    });
    let err = parse_openai_response(&json).unwrap_err().to_string();
    assert!(err.contains("Empty choices array"));
    assert!(err.contains("valid payload"));
}

#[test]
fn test_parse_openai_response_with_tool_calls() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\": \"test.txt\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gpt-4o"
    });
    let resp = parse_openai_response(&json).unwrap();
    let tc = resp.message.tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].function.name, "read_file");
}

#[test]
fn test_parse_openai_response_accepts_object_valued_tool_arguments() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_dict_args",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": {"path": "README.md"}
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "local-openai-compatible"
    });

    let resp = parse_openai_response(&json).expect("object arguments should parse");
    let tc = resp.message.tool_calls.as_ref().unwrap();
    let args: Value = serde_json::from_str(&tc[0].function.arguments).unwrap();
    assert_eq!(args["path"], "README.md");
}

#[test]
fn test_parse_openai_response_with_tool_call_extra_content() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\": \"test.txt\"}"
                    },
                    "extra_content": {
                        "google": {
                            "thought_signature": "SIG_ABC123"
                        }
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gemini-2.5-pro"
    });
    let resp = parse_openai_response(&json).unwrap();
    let tc = resp.message.tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].function.name, "read_file");
    assert_eq!(
        tc[0].extra_content,
        Some(serde_json::json!({
            "google": {
                "thought_signature": "SIG_ABC123"
            }
        }))
    );
}

#[test]
fn test_parse_sse_chunk_content() {
    let json = serde_json::json!({
        "choices": [{
            "delta": {
                "content": "Hello"
            },
            "finish_reason": null
        }]
    });
    let chunk = parse_sse_chunk(&json).unwrap();
    assert_eq!(
        chunk.delta.as_ref().unwrap().content.as_deref(),
        Some("Hello")
    );
    assert!(chunk.finish_reason.is_none());
}

#[test]
fn test_parse_sse_chunk_tool_call() {
    let json = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_abc",
                    "function": {
                        "name": "search",
                        "arguments": ""
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let chunk = parse_sse_chunk(&json).unwrap();
    let tc = chunk.delta.as_ref().unwrap().tool_calls.as_ref().unwrap();
    assert_eq!(tc[0].index, 0);
    assert_eq!(tc[0].id.as_deref(), Some("call_abc"));
}

#[test]
fn test_parse_sse_chunk_finish() {
    let json = serde_json::json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        }
    });
    let chunk = parse_sse_chunk(&json).unwrap();
    assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
    assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 150);
}

#[test]
fn test_anthropic_convert_messages() {
    let messages = vec![
        Message::system("You are helpful"),
        Message::user("Hello"),
        Message::assistant("Hi there!"),
    ];
    let (system, msgs) = AnthropicProvider::convert_messages(&messages, None);
    assert_eq!(system.as_deref(), Some("You are helpful"));
    assert_eq!(msgs.len(), 2); // user + assistant, system extracted
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");
}

#[test]
fn test_anthropic_convert_messages_decodes_acp_multimodal_user_parts() {
    let parts = serde_json::json!([
        {"type": "text", "text": "see attachment"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
    ]);
    let messages = vec![Message::user(format!("{ACP_MULTIMODAL_PREFIX}{parts}"))];
    let (_, msgs) = AnthropicProvider::convert_messages(&messages, None);
    let blocks = msgs[0]["content"].as_array().expect("blocks");
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[1]["type"], "image");
}

#[test]
fn test_anthropic_convert_messages_with_tool_result() {
    let messages = vec![
        Message::system("System"),
        Message::user("Do something"),
        Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc_1".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"test.txt"}"#.to_string(),
                },
                extra_content: None,
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            anthropic_content_blocks: None,
            cache_control: None,
        },
        Message::tool_result("tc_1", "file contents here"),
    ];
    let (system, msgs) = AnthropicProvider::convert_messages(&messages, None);
    assert_eq!(system.as_deref(), Some("System"));
    assert_eq!(msgs.len(), 3); // user, assistant with tool_use, user with tool_result
                               // Assistant message should have tool_use block
    let assistant_content = msgs[1]["content"].as_array().unwrap();
    assert_eq!(assistant_content[0]["type"], "tool_use");
    assert_eq!(assistant_content[0]["name"], "read_file");
    // Tool result should be a user message with tool_result block
    let tool_content = msgs[2]["content"].as_array().unwrap();
    assert_eq!(tool_content[0]["type"], "tool_result");
    assert_eq!(tool_content[0]["tool_use_id"], "tc_1");
}

#[test]
fn test_anthropic_convert_messages_preserves_ordered_content_blocks() {
    let ordered_blocks = vec![
        serde_json::json!({"type": "thinking", "thinking": "first", "signature": "sig-1"}),
        serde_json::json!({"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.py"}}),
        serde_json::json!({"type": "thinking", "thinking": "second", "signature": "sig-2"}),
        serde_json::json!({"type": "tool_use", "id": "toolu_2", "name": "read_file", "input": {"path": "b.py"}}),
    ];
    let messages = vec![Message {
        role: MessageRole::Assistant,
        content: None,
        tool_calls: Some(vec![
            ToolCall {
                id: "toolu_1".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"a.py"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "toolu_2".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"b.py"}"#.to_string(),
                },
                extra_content: None,
            },
        ]),
        tool_call_id: None,
        name: None,
        reasoning_content: Some("first\nsecond".to_string()),
        anthropic_content_blocks: Some(ordered_blocks.clone()),
        cache_control: None,
    }];

    let (_, msgs) = AnthropicProvider::convert_messages(&messages, None);
    let content = msgs[0]["content"].as_array().unwrap();
    let order: Vec<(&str, String)> = content
        .iter()
        .map(|block| {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
            let key = block
                .get("signature")
                .or_else(|| block.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (block_type, key)
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("thinking", "sig-1".to_string()),
            ("tool_use", "toolu_1".to_string()),
            ("thinking", "sig-2".to_string()),
            ("tool_use", "toolu_2".to_string()),
        ]
    );
}

#[test]
fn test_anthropic_convert_messages_kimi_tool_replay_preserves_reasoning_content() {
    let messages = vec![Message {
        role: MessageRole::Assistant,
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc_kimi".to_string(),
            function: FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"date"}"#.to_string(),
            },
            extra_content: None,
        }]),
        tool_call_id: None,
        name: None,
        reasoning_content: Some("provider scratchpad".to_string()),
        anthropic_content_blocks: None,
        cache_control: None,
    }];
    let (_, msgs) =
        AnthropicProvider::convert_messages(&messages, Some("https://api.kimi.com/coding/v1"));
    let content = msgs[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "provider scratchpad");
    assert_eq!(content[1]["type"], "tool_use");
}

#[test]
fn test_anthropic_convert_messages_kimi_accepts_empty_reasoning_content() {
    let messages = vec![Message {
        role: MessageRole::Assistant,
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc_empty".to_string(),
            function: FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
            extra_content: None,
        }]),
        tool_call_id: None,
        name: None,
        reasoning_content: Some(String::new()),
        anthropic_content_blocks: None,
        cache_control: None,
    }];
    let (_, msgs) =
        AnthropicProvider::convert_messages(&messages, Some("https://api.moonshot.ai/v1"));
    let content = msgs[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "");
}

#[test]
fn test_anthropic_convert_messages_non_kimi_skips_thinking_block() {
    let messages = vec![Message {
        role: MessageRole::Assistant,
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc_other".to_string(),
            function: FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            },
            extra_content: None,
        }]),
        tool_call_id: None,
        name: None,
        reasoning_content: Some("scratchpad".to_string()),
        anthropic_content_blocks: None,
        cache_control: None,
    }];
    let (_, msgs) =
        AnthropicProvider::convert_messages(&messages, Some("https://api.anthropic.com"));
    let content = msgs[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "tool_use");
}

#[test]
fn test_anthropic_messages_url_adds_azure_api_version_query() {
    let url = AnthropicProvider::messages_url_for_base_url(
        "https://my-resource.openai.azure.com/anthropic",
    );
    assert_eq!(
        url,
        "https://my-resource.openai.azure.com/anthropic/v1/messages?api-version=2025-04-15"
    );

    let existing = AnthropicProvider::messages_url_for_base_url(
        "https://my-resource.openai.azure.com/anthropic?api-version=2024-01-01",
    );
    assert_eq!(
        existing,
        "https://my-resource.openai.azure.com/anthropic/v1/messages?api-version=2024-01-01"
    );
}

#[test]
fn test_anthropic_request_uses_bearer_auth_and_azure_betas_for_foundry() {
    let provider = AnthropicProvider::new("azure-foundry-secret")
        .with_base_url("https://my-resource.openai.azure.com/anthropic");
    let url = provider.messages_url();
    let request = provider
        .build_request(
            &Client::new(),
            &url,
            "azure-foundry-secret",
            &serde_json::json!({}),
        )
        .build()
        .expect("request");
    let headers = request.headers();
    assert_eq!(
        headers.get("Authorization").and_then(|h| h.to_str().ok()),
        Some("Bearer azure-foundry-secret")
    );
    assert!(headers.get("x-api-key").is_none());
    let betas = headers
        .get("anthropic-beta")
        .and_then(|h| h.to_str().ok())
        .expect("anthropic-beta");
    assert!(betas.contains("context-1m-2025-08-07"));
    assert!(betas.contains("fine-grained-tool-streaming-2025-05-14"));
}

#[test]
fn test_anthropic_request_uses_api_key_for_native_api_key() {
    let provider = AnthropicProvider::new("sk-ant-api03-secret");
    let request = provider
        .build_request(
            &Client::new(),
            "https://api.anthropic.com/v1/messages",
            "sk-ant-api03-secret",
            &serde_json::json!({}),
        )
        .build()
        .expect("request");
    let headers = request.headers();
    assert_eq!(
        headers.get("x-api-key").and_then(|h| h.to_str().ok()),
        Some("sk-ant-api03-secret")
    );
    assert!(headers.get("Authorization").is_none());
    let betas = headers
        .get("anthropic-beta")
        .and_then(|h| h.to_str().ok())
        .expect("anthropic-beta");
    assert!(betas.contains("interleaved-thinking-2025-05-14"));
    assert!(!betas.contains("oauth-2025-04-20"));
    assert!(!betas.contains("context-1m-2025-08-07"));
}

#[test]
fn test_anthropic_request_uses_bearer_and_oauth_betas_for_native_oauth() {
    let provider = AnthropicProvider::new("sk-ant-oat01-secret");
    let request = provider
        .build_request(
            &Client::new(),
            "https://api.anthropic.com/v1/messages",
            "sk-ant-oat01-secret",
            &serde_json::json!({}),
        )
        .build()
        .expect("request");
    let headers = request.headers();
    assert_eq!(
        headers.get("Authorization").and_then(|h| h.to_str().ok()),
        Some("Bearer sk-ant-oat01-secret")
    );
    assert!(headers.get("x-api-key").is_none());
    let betas = headers
        .get("anthropic-beta")
        .and_then(|h| h.to_str().ok())
        .expect("anthropic-beta");
    assert!(betas.contains("oauth-2025-04-20"));
    assert!(betas.contains("claude-code-20250219"));
}

#[test]
fn anthropic_provider_request_timeout_survives_client_rebuilds() {
    let provider = AnthropicProvider::new("sk-ant-api03-secret").with_request_timeout_seconds(90.0);

    assert_eq!(
        provider.configured_request_timeout(),
        Some(Duration::from_secs(90))
    );

    provider.refresh_client("unit test");
    assert_eq!(
        provider.configured_request_timeout(),
        Some(Duration::from_secs(90))
    );
}

#[test]
fn test_anthropic_strips_sampling_and_unsupported_fast_controls() {
    let mut body = serde_json::json!({
        "model": "claude-opus-4-8-fast",
        "messages": [],
        "temperature": 0.7,
        "top_p": 0.9,
        "top_k": 20,
        "speed": "fast"
    });
    AnthropicProvider::strip_unsupported_anthropic_controls(&mut body, "claude-opus-4-8-fast");
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("top_k").is_none());
    assert!(body.get("speed").is_none());

    let mut supported = serde_json::json!({
        "model": "claude-opus-4-6",
        "messages": [],
        "temperature": 0.7,
        "speed": "fast"
    });
    AnthropicProvider::strip_unsupported_anthropic_controls(&mut supported, "claude-opus-4-6");
    assert_eq!(supported["temperature"], serde_json::json!(0.7));
    assert_eq!(supported["speed"], serde_json::json!("fast"));
}

#[test]
fn test_anthropic_parse_response() {
    let json = serde_json::json!({
        "content": [
            {"type": "text", "text": "Here is the answer."}
        ],
        "model": "claude-3-5-sonnet-20241022",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 100,
            "output_tokens": 50
        }
    });
    let resp = AnthropicProvider::parse_response(&json).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("Here is the answer."));
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    assert_eq!(resp.usage.as_ref().unwrap().prompt_tokens, 100);
    assert_eq!(resp.usage.as_ref().unwrap().completion_tokens, 50);
}

#[test]
fn test_anthropic_parse_response_preserves_thinking_as_reasoning_content() {
    let json = serde_json::json!({
        "content": [
            {"type": "thinking", "thinking": "step 1"},
            {"type": "text", "text": "answer"}
        ],
        "model": "claude-3-5-sonnet-20241022",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5
        }
    });
    let resp = AnthropicProvider::parse_response(&json).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("answer"));
    assert_eq!(resp.message.reasoning_content.as_deref(), Some("step 1"));
}

#[test]
fn test_anthropic_parse_response_preserves_interleaved_content_blocks() {
    let json = serde_json::json!({
        "content": [
            {"type": "thinking", "thinking": "first", "signature": "sig-1"},
            {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.py"}},
            {"type": "redacted_thinking", "data": "ciphertext"},
            {"type": "tool_use", "id": "toolu_2", "name": "read_file", "input": {"path": "b.py"}}
        ],
        "model": "claude-opus-4-8",
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5
        }
    });
    let resp = AnthropicProvider::parse_response(&json).unwrap();
    let blocks = resp
        .message
        .anthropic_content_blocks
        .as_ref()
        .expect("ordered blocks");
    assert_eq!(blocks.len(), 4);
    assert_eq!(blocks[0]["signature"], "sig-1");
    assert_eq!(blocks[1]["id"], "toolu_1");
    assert_eq!(blocks[2]["type"], "redacted_thinking");
    assert_eq!(blocks[3]["id"], "toolu_2");
}

#[test]
fn test_anthropic_parse_response_with_tool_use() {
    let json = serde_json::json!({
        "content": [
            {"type": "text", "text": "Let me read that file."},
            {
                "type": "tool_use",
                "id": "toolu_123",
                "name": "read_file",
                "input": {"path": "test.txt"}
            }
        ],
        "model": "claude-3-5-sonnet-20241022",
        "stop_reason": "tool_use",
        "usage": {
            "input_tokens": 200,
            "output_tokens": 80
        }
    });
    let resp = AnthropicProvider::parse_response(&json).unwrap();
    assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
    let tc = resp.message.tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].id, "toolu_123");
    assert_eq!(tc[0].function.name, "read_file");
}

#[test]
fn test_openrouter_parse_response_with_reasoning() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "The answer is 42.",
                "reasoning_content": "Let me think step by step..."
            },
            "finish_reason": "stop"
        }],
        "model": "deepseek/deepseek-r1",
        "usage": {
            "prompt_tokens": 50,
            "completion_tokens": 30,
            "total_tokens": 80
        }
    });
    let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("The answer is 42."));
    assert_eq!(
        resp.message.reasoning_content.as_deref(),
        Some("Let me think step by step...")
    );
}

#[test]
fn test_openrouter_parse_response_with_reasoning_details() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Final answer.",
                "reasoning_details": [
                    {"type": "text", "text": "Step 1"},
                    {"type": "text", "text": "Step 2"}
                ]
            },
            "finish_reason": "stop"
        }],
        "model": "openai/o1-preview"
    });
    let resp = OpenRouterProvider::parse_openrouter_response(&json).unwrap();
    let reasoning = resp.message.reasoning_content.as_deref().unwrap();
    assert!(reasoning.contains("Step 1"));
    assert!(reasoning.contains("Step 2"));
}

#[test]
fn test_openrouter_parse_response_null_content_preserves_reasoning() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "reasoning_content": "Tool-only reasoning path"
            },
            "finish_reason": "stop"
        }],
        "model": "deepseek/deepseek-r1"
    });

    let resp = OpenRouterProvider::parse_openrouter_response(&json)
        .expect("reasoning-only OpenRouter response should parse");

    assert_eq!(resp.message.content.as_deref(), Some(""));
    assert_eq!(
        resp.message.reasoning_content.as_deref(),
        Some("Tool-only reasoning path")
    );
}

#[test]
fn test_openrouter_build_headers() {
    let provider = OpenRouterProvider::new("key")
        .with_http_referer("https://example.com")
        .with_x_title("My App");
    let headers = provider.build_headers();
    assert!(headers
        .iter()
        .any(|(k, v)| k == "HTTP-Referer" && v == "https://example.com"));
    assert!(headers.iter().any(|(k, v)| k == "X-Title" && v == "My App"));
}

#[test]
fn test_openrouter_parse_response_cache_control_from_extra_body() {
    let extra = serde_json::json!({
        "response_cache": {
            "enabled": true,
            "ttl_secs": 42,
            "clear": false
        }
    });
    let control = OpenRouterProvider::parse_response_cache_control(Some(&extra));
    assert!(control.enabled);
    assert_eq!(control.ttl_secs, 42);
    assert!(!control.clear);
}

#[test]
fn test_openrouter_merge_extra_body_strips_local_cache_fields() {
    let extra = serde_json::json!({
        "response_cache": {"enabled": true},
        "response_cache_enabled": true,
        "response_cache_ttl_secs": 30,
        "response_cache_clear": false,
        "strict_api": true,
        "strict_tool_calls": true,
        "provider_strict": true,
        "reasoning_effort": "high",
        "route": "fallback",
        "provider": {"order": ["openai"]}
    });
    let merged = OpenRouterProvider::merge_extra_body(Some(&extra)).expect("merged body");
    assert!(merged.get("response_cache").is_none());
    assert!(merged.get("response_cache_enabled").is_none());
    assert!(merged.get("response_cache_ttl_secs").is_none());
    assert!(merged.get("response_cache_clear").is_none());
    assert!(merged.get("strict_api").is_none());
    assert!(merged.get("strict_tool_calls").is_none());
    assert!(merged.get("provider_strict").is_none());
    assert!(merged.get("reasoning_effort").is_none());
    assert_eq!(merged["reasoning"]["effort"], "high");
    assert_eq!(
        merged.get("route").and_then(|v| v.as_str()),
        Some("fallback")
    );
    assert!(merged.get("provider").is_some());
}

#[test]
fn test_anthropic_convert_tools() {
    let tools = vec![ToolSchema::new(
        "read_file",
        "Read a file",
        hermes_core::JsonSchema::new("object"),
    )];
    let converted = AnthropicProvider::convert_tools(&tools);
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["name"], "read_file");
    assert_eq!(converted[0]["description"], "Read a file");
    assert!(converted[0].get("input_schema").is_some());
}

#[test]
fn test_anthropic_resolve_messages_max_tokens_prefers_positive_request() {
    let resolved = AnthropicProvider::resolve_messages_max_tokens(Some(8192), "claude-opus-4-1");
    assert_eq!(resolved, 8192);
}

#[test]
fn test_anthropic_resolve_messages_max_tokens_zero_falls_back_to_model_default() {
    let resolved = AnthropicProvider::resolve_messages_max_tokens(Some(0), "claude-opus-4-6");
    assert!(resolved > 0);
    assert_eq!(resolved, get_anthropic_max_output("claude-opus-4-6"));
}

#[test]
fn test_anthropic_resolve_messages_max_tokens_none_falls_back_to_model_default() {
    let resolved = AnthropicProvider::resolve_messages_max_tokens(None, "claude-sonnet-4-6");
    assert!(resolved > 0);
    assert_eq!(resolved, get_anthropic_max_output("claude-sonnet-4-6"));
}
