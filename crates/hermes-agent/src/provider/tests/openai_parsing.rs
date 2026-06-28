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
