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
