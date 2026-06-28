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
