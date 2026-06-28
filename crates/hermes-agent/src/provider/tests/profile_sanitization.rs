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
