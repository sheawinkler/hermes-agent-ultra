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
