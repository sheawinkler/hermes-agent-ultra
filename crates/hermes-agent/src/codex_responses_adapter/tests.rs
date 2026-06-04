use super::*;
use serde_json::json;

fn msgs_multimodal_tool_result() -> Vec<Value> {
    json!([
        {"role": "user", "content": "What's in /tmp/foo.png?"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_abc",
                "type": "function",
                "function": {
                    "name": "vision_analyze",
                    "arguments": "{\"image_url\": \"/tmp/foo.png\", \"question\": \"describe\"}",
                },
            }],
        },
        {
            "role": "tool",
            "name": "vision_analyze",
            "tool_call_id": "call_abc",
            "content": [
                {"type": "text", "text": "Image loaded."},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,XYZ"}},
            ],
        },
    ])
    .as_array()
    .unwrap()
    .clone()
}

#[test]
fn summarize_plain_string_passthrough() {
    assert_eq!(
        summarize_user_message_for_log(Some(&json!("hello world"))),
        "hello world"
    );
}

#[test]
fn summarize_none_returns_empty() {
    assert_eq!(summarize_user_message_for_log(None), "");
    assert_eq!(summarize_user_message_for_log(Some(&Value::Null)), "");
}

#[test]
fn chat_content_non_list_returns_empty() {
    assert!(chat_content_to_responses_parts(&json!("hi"), "user").is_empty());
    assert!(chat_content_to_responses_parts(&Value::Null, "user").is_empty());
}

#[test]
fn chat_content_text_becomes_input_text() {
    let content = json!([{"type": "text", "text": "hello"}]);
    assert_eq!(
        chat_content_to_responses_parts(&content, "user"),
        vec![json!({"type": "input_text", "text": "hello"})]
    );
}

#[test]
fn chat_content_assistant_uses_output_text() {
    let content = json!([{"type": "text", "text": "I found the files."}]);
    let parts = chat_content_to_responses_parts(&content, "assistant");
    assert_eq!(parts[0]["type"], "output_text");
}

#[test]
fn chat_content_image_url_object() {
    let content = json!([{"type": "image_url", "image_url": {"url": "https://x", "detail": "high"}}]);
    assert_eq!(
        chat_content_to_responses_parts(&content, "user"),
        vec![json!({"type": "input_image", "image_url": "https://x", "detail": "high"})]
    );
}

#[test]
fn multimodal_tool_result_becomes_output_array() {
    let items = chat_messages_to_responses_input(&msgs_multimodal_tool_result(), true, None);
    let outputs: Vec<_> = items
        .iter()
        .filter(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
        .collect();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0]["call_id"], "call_abc");
    assert!(outputs[0]["output"].is_array());
    let types: Vec<_> = outputs[0]["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.get("type").and_then(|v| v.as_str()))
        .collect();
    assert!(types.contains(&"input_text"));
    assert!(types.contains(&"input_image"));
}

#[test]
fn multimodal_tool_result_preserves_data_url() {
    let items = chat_messages_to_responses_input(&msgs_multimodal_tool_result(), true, None);
    let out = items
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
        .unwrap();
    let image_parts: Vec<_> = out["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.get("type").and_then(|v| v.as_str()) == Some("input_image"))
        .collect();
    assert_eq!(image_parts.len(), 1);
    assert_eq!(image_parts[0]["image_url"], "data:image/png;base64,XYZ");
}

#[test]
fn string_tool_content_stays_string_output() {
    let msgs = json!([
        {"role": "user", "content": "hi"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_x", "type": "function",
                "function": {"name": "terminal", "arguments": "{}"},
            }],
        },
        {"role": "tool", "name": "terminal", "tool_call_id": "call_x", "content": "ls output here"},
    ]);
    let items = chat_messages_to_responses_input(msgs.as_array().unwrap(), true, None);
    let out = items
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
        .unwrap();
    assert!(out["output"].is_string());
    assert_eq!(out["output"], "ls output here");
}

#[test]
fn chat_messages_uses_call_id_for_function_call() {
    let msgs = json!([
        {"role": "user", "content": "Run terminal"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_abc123",
                "type": "function",
                "function": {"name": "terminal", "arguments": "{}"},
            }],
        },
        {"role": "tool", "tool_call_id": "call_abc123", "content": "{\"ok\":true}"},
    ]);
    let items = chat_messages_to_responses_input(msgs.as_array().unwrap(), true, None);
    let fc = items
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call"))
        .unwrap();
    let fo = items
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
        .unwrap();
    assert_eq!(fc["call_id"], "call_abc123");
    assert!(fc.get("id").is_none());
    assert_eq!(fo["call_id"], "call_abc123");
}

#[test]
fn chat_messages_accepts_call_pipe_fc_ids() {
    let msgs = json!([
        {"role": "user", "content": "Run terminal"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_pair123|fc_pair123",
                "type": "function",
                "function": {"name": "terminal", "arguments": "{}"},
            }],
        },
        {"role": "tool", "tool_call_id": "call_pair123|fc_pair123", "content": "{\"ok\":true}"},
    ]);
    let items = chat_messages_to_responses_input(msgs.as_array().unwrap(), true, None);
    let fc = items
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call"))
        .unwrap();
    assert_eq!(fc["call_id"], "call_pair123");
}

#[test]
fn preflight_strips_function_call_id() {
    let kwargs = json!({
        "model": "gpt-5-codex",
        "instructions": "You are Hermes.",
        "input": [
            {"role": "user", "content": "hi"},
            {
                "type": "function_call",
                "id": "call_bad",
                "call_id": "call_good",
                "name": "terminal",
                "arguments": "{}",
            },
        ],
        "tools": [],
        "store": false,
    });
    let preflight = preflight_codex_api_kwargs(&kwargs, false).unwrap();
    let fc = preflight["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call"))
        .unwrap();
    assert_eq!(fc["call_id"], "call_good");
    assert!(fc.get("id").is_none());
}

#[test]
fn preflight_rejects_function_call_output_without_call_id() {
    let kwargs = json!({
        "model": "gpt-5-codex",
        "instructions": "You are Hermes.",
        "input": [{"type": "function_call_output", "output": "{}"}],
        "tools": [],
        "store": false,
    });
    let err = preflight_codex_api_kwargs(&kwargs, false).unwrap_err();
    assert!(matches!(err, CodexAdapterError::ValueError(msg) if msg.contains("function_call_output is missing call_id")));
}

#[test]
fn preflight_rejects_unsupported_request_fields() {
    let mut kwargs = json!({
        "model": "gpt-5-codex",
        "instructions": "You are Hermes.",
        "input": [{"role": "user", "content": "Ping"}],
        "tools": null,
        "store": false,
    });
    kwargs
        .as_object_mut()
        .unwrap()
        .insert("some_unknown_field".to_string(), json!("value"));
    let err = preflight_codex_api_kwargs(&kwargs, false).unwrap_err();
    assert!(matches!(err, CodexAdapterError::ValueError(msg) if msg.contains("unsupported field")));
}

#[test]
fn preflight_allows_reasoning_and_temperature() {
    let kwargs = json!({
        "model": "gpt-5-codex",
        "instructions": "You are Hermes.",
        "input": [{"role": "user", "content": "Ping"}],
        "store": false,
        "reasoning": {"effort": "high", "summary": "auto"},
        "include": ["reasoning.encrypted_content"],
        "temperature": 0.7,
        "max_output_tokens": 4096,
    });
    let result = preflight_codex_api_kwargs(&kwargs, false).unwrap();
    assert_eq!(result["reasoning"], json!({"effort": "high", "summary": "auto"}));
    assert_eq!(result["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(result["temperature"], 0.7);
    assert_eq!(result["max_output_tokens"], 4096);
}

#[test]
fn preflight_array_output_passthrough() {
    let raw = json!([
        {"type": "function_call", "call_id": "call_abc", "name": "vision_analyze", "arguments": "{}"},
        {
            "type": "function_call_output",
            "call_id": "call_abc",
            "output": [
                {"type": "input_text", "text": "Image loaded."},
                {"type": "input_image", "image_url": "data:image/png;base64,ABC"},
            ],
        },
    ]);
    let normalized = preflight_codex_input_items(&raw).unwrap();
    let out = normalized
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
        .unwrap();
    assert!(out["output"].is_array());
    assert_eq!(out["output"][1]["image_url"], "data:image/png;base64,ABC");
}

#[test]
fn preflight_drops_unknown_part_types() {
    let raw = json!([
        {"type": "function_call", "call_id": "call_abc", "name": "vision_analyze", "arguments": "{}"},
        {
            "type": "function_call_output",
            "call_id": "call_abc",
            "output": [
                {"type": "input_text", "text": "ok"},
                {"type": "garbage", "data": "nope"},
                {"type": "input_image", "image_url": "data:image/png;base64,ZZ"},
            ],
        },
    ]);
    let normalized = preflight_codex_input_items(&raw).unwrap();
    let out = normalized
        .iter()
        .find(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
        .unwrap();
    let types: Vec<_> = out["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.get("type").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(types, vec!["input_text", "input_image"]);
}

#[test]
fn normalize_commentary_only_is_incomplete() {
    let response = json!({
        "output": [{
            "type": "message",
            "phase": "commentary",
            "status": "completed",
            "content": [{"type": "output_text", "text": "I'll inspect the repository first."}],
        }],
        "status": "completed",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "incomplete");
    assert!(msg.content.contains("inspect the repository"));
}

#[test]
fn normalize_preserves_message_status_for_replay() {
    let response = json!({
        "output": [{
            "type": "message",
            "id": "msg_partial",
            "phase": "commentary",
            "status": "in_progress",
            "content": [{"type": "output_text", "text": "Still working..."}],
        }],
        "status": "in_progress",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "incomplete");
    let items = msg.codex_message_items.as_ref().unwrap();
    assert_eq!(items[0]["id"], "msg_partial");
    assert_eq!(items[0]["status"], "in_progress");
}

#[test]
fn normalize_detects_leaked_tool_call_text() {
    let leaked = "I'll check the official page directly.\n\
        to=functions.exec_command {\"cmd\": \"curl https://example.test\"}\n\
        assistant to=functions.exec_command {\"stdout\": \"mailto:foo@example.test\"}\n\
        Extracted: foo@example.test";
    let response = json!({
        "output": [{
            "type": "message",
            "status": "completed",
            "content": [{"type": "output_text", "text": leaked}],
        }],
        "status": "completed",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "incomplete");
    assert!(msg.content.is_empty());
    assert!(msg.tool_calls.is_empty());
}

#[test]
fn normalize_keeps_content_when_real_tool_call_present() {
    let response = json!({
        "output": [
            {
                "type": "message",
                "status": "completed",
                "content": [{"type": "output_text", "text": "Running the command via to=functions.exec_command now."}],
            },
            {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "terminal",
                "arguments": "{}",
            },
        ],
        "status": "completed",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "tool_calls");
    assert!(!msg.tool_calls.is_empty());
    assert!(msg.content.contains("Running the command"));
}

#[test]
fn normalize_no_leak_passes_through() {
    let response = json!({
        "output": [{
            "type": "message",
            "status": "completed",
            "content": [{"type": "output_text", "text": "Here is the answer with no leak."}],
        }],
        "status": "completed",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "stop");
    assert_eq!(msg.content, "Here is the answer with no leak.");
}

#[test]
fn normalize_reasoning_only_is_incomplete() {
    let response = json!({
        "output": [{
            "type": "reasoning",
            "id": "rs_001",
            "encrypted_content": "enc_abc123",
            "summary": [{"text": "Thinking..."}],
            "status": "completed",
        }],
        "status": "completed",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "incomplete");
    assert!(msg.content.is_empty());
    let items = msg.codex_reasoning_items.as_ref().unwrap();
    assert_eq!(items[0]["encrypted_content"], "enc_abc123");
}

#[test]
fn normalize_reasoning_with_content_is_stop() {
    let response = json!({
        "output": [
            {
                "type": "reasoning",
                "id": "rs_001",
                "encrypted_content": "enc_xyz",
                "summary": [{"text": "Thinking..."}],
                "status": "completed",
            },
            {
                "type": "message",
                "content": [{"type": "output_text", "text": "Here is the answer."}],
                "status": "completed",
            },
        ],
        "status": "completed",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "stop");
    assert!(msg.content.contains("Here is the answer"));
}

#[test]
fn reasoning_only_has_following_assistant_item() {
    let messages = json!([
        {"role": "user", "content": "think hard"},
        {
            "role": "assistant",
            "content": "",
            "finish_reason": "incomplete",
            "codex_reasoning_items": [
                {"type": "reasoning", "id": "rs_001", "encrypted_content": "enc_abc", "summary": []},
            ],
        },
    ]);
    let items = chat_messages_to_responses_input(messages.as_array().unwrap(), true, None);
    let ri_idx = items
        .iter()
        .position(|it| it.get("type").and_then(|v| v.as_str()) == Some("reasoning"))
        .unwrap();
    assert!(ri_idx < items.len() - 1);
    assert_eq!(items[ri_idx + 1]["role"], "assistant");
}

#[test]
fn chat_messages_deduplicates_reasoning_ids() {
    let messages = json!([
        {"role": "user", "content": "think hard"},
        {
            "role": "assistant",
            "content": "",
            "codex_reasoning_items": [
                {"type": "reasoning", "id": "rs_aaa", "encrypted_content": "enc_1"},
                {"type": "reasoning", "id": "rs_bbb", "encrypted_content": "enc_2"},
            ],
        },
        {
            "role": "assistant",
            "content": "partial answer",
            "codex_reasoning_items": [
                {"type": "reasoning", "id": "rs_aaa", "encrypted_content": "enc_1"},
                {"type": "reasoning", "id": "rs_ccc", "encrypted_content": "enc_3"},
            ],
        },
    ]);
    let items = chat_messages_to_responses_input(messages.as_array().unwrap(), true, None);
    let reasoning: Vec<_> = items
        .iter()
        .filter(|it| it.get("type").and_then(|v| v.as_str()) == Some("reasoning"))
        .collect();
    assert_eq!(reasoning.len(), 3);
    let encrypted: Vec<_> = reasoning
        .iter()
        .filter_map(|it| it.get("encrypted_content").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(encrypted.iter().filter(|e| **e == "enc_1").count(), 1);
    for it in &reasoning {
        assert!(it.get("id").is_none());
    }
}

#[test]
fn cross_issuer_reasoning_dropped_on_replay() {
    let messages = json!([
        {"role": "user", "content": "hi"},
        {
            "role": "assistant",
            "content": "hi",
            "codex_reasoning_items": [{
                "type": "reasoning",
                "id": "rs_001",
                "encrypted_content": "grok_blob",
                "summary": [],
                "_issuer_kind": "xai_responses",
            }],
        },
        {"role": "user", "content": "next"},
    ]);
    let items = chat_messages_to_responses_input(
        messages.as_array().unwrap(),
        true,
        Some("codex_backend"),
    );
    let reasoning: Vec<_> = items
        .iter()
        .filter(|it| it.get("type").and_then(|v| v.as_str()) == Some("reasoning"))
        .collect();
    assert!(reasoning.is_empty());
}

#[test]
fn same_issuer_reasoning_replayed() {
    let messages = json!([
        {"role": "user", "content": "hi"},
        {
            "role": "assistant",
            "content": "hi",
            "codex_reasoning_items": [{
                "type": "reasoning",
                "id": "rs_001",
                "encrypted_content": "grok_blob",
                "summary": [],
                "_issuer_kind": "xai_responses",
            }],
        },
        {"role": "user", "content": "next"},
    ]);
    let items = chat_messages_to_responses_input(
        messages.as_array().unwrap(),
        true,
        Some("xai_responses"),
    );
    let reasoning: Vec<_> = items
        .iter()
        .filter(|it| it.get("type").and_then(|v| v.as_str()) == Some("reasoning"))
        .collect();
    assert_eq!(reasoning.len(), 1);
    assert_eq!(reasoning[0]["encrypted_content"], "grok_blob");
    assert!(reasoning[0].get("_issuer_kind").is_none());
}

#[test]
fn normalize_stamps_issuer_on_reasoning() {
    let response = json!({
        "output": [
            {
                "type": "reasoning",
                "id": "rs_new",
                "encrypted_content": "fresh_blob",
                "summary": [],
            },
            {
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "ok"}],
                "id": "msg_1",
            },
        ],
        "status": "completed",
    });
    let (msg, _) = normalize_codex_response(&response, Some("xai_responses")).unwrap();
    let items = msg.codex_reasoning_items.as_ref().unwrap();
    assert_eq!(items[0]["_issuer_kind"], "xai_responses");
}

#[test]
fn normalize_drops_transient_rs_tmp_reasoning() {
    let response = json!({
        "output": [
            {
                "type": "reasoning",
                "id": "rs_tmp_123",
                "encrypted_content": "opaque-transient",
                "summary": [],
            },
            {
                "type": "reasoning",
                "id": "rs_456",
                "encrypted_content": "opaque-stable",
                "summary": [{"text": "stable summary"}],
            },
            {
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "done"}],
            },
        ],
        "status": "completed",
    });
    let (msg, finish) = normalize_codex_response(&response, None).unwrap();
    assert_eq!(finish, "stop");
    assert_eq!(msg.content, "done");
    let items = msg.codex_reasoning_items.as_ref().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "rs_456");
}

#[test]
fn format_responses_error_combines_code_and_message() {
    let err = json!({"code": "rate_limit_exceeded", "message": "Slow down"});
    assert_eq!(
        format_responses_error(Some(&err), "failed"),
        "rate_limit_exceeded: Slow down"
    );
}

#[test]
fn format_responses_error_falls_back_to_status() {
    assert_eq!(
        format_responses_error(None, "failed"),
        "Responses API returned status 'failed'"
    );
}

#[test]
fn normalize_failed_includes_code_in_error() {
    let response = json!({
        "status": "failed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "status": "incomplete",
            "content": [{"type": "output_text", "text": "partial"}],
        }],
        "error": {"code": "rate_limit_exceeded", "message": "Slow down"},
    });
    let err = normalize_codex_response(&response, None).unwrap_err();
    assert!(matches!(err, CodexAdapterError::RuntimeError(msg) if msg == "rate_limit_exceeded: Slow down"));
}

#[test]
fn classify_issuer_variants() {
    assert_eq!(
        classify_responses_issuer(true, false, false, None),
        "xai_responses"
    );
    assert_eq!(
        classify_responses_issuer(false, true, false, None),
        "github_responses"
    );
    assert_eq!(
        classify_responses_issuer(false, false, true, None),
        "codex_backend"
    );
    assert_eq!(
        classify_responses_issuer(false, false, false, Some("https://example.com")),
        "other:https://example.com"
    );
}

#[test]
fn deterministic_call_id_is_stable() {
    let a = deterministic_call_id("terminal", "{}", 0);
    let b = deterministic_call_id("terminal", "{}", 0);
    assert_eq!(a, b);
    assert!(a.starts_with("call_"));
}

#[test]
fn derive_responses_function_call_id_from_fc_prefix() {
    assert_eq!(
        derive_responses_function_call_id("call_abc", Some("fc_xyz")),
        "fc_xyz"
    );
}

#[test]
fn split_responses_tool_id_pipe_form() {
    let (call, fc) = split_responses_tool_id(Some("call_pair|fc_pair"));
    assert_eq!(call.as_deref(), Some("call_pair"));
    assert_eq!(fc.as_deref(), Some("fc_pair"));
}
