use super::*;
use hermes_core::JsonSchema;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;

fn encode_event_stream_message(event_type: &str, payload: Value) -> Vec<u8> {
    let payload = serde_json::to_vec(&payload).expect("payload JSON");
    let mut headers = Vec::new();
    push_event_stream_string_header(&mut headers, ":message-type", "event");
    push_event_stream_string_header(&mut headers, ":event-type", event_type);
    push_event_stream_string_header(&mut headers, ":content-type", "application/json");
    let total_len = 16 + headers.len() + payload.len();
    let mut frame = Vec::with_capacity(total_len);
    frame.extend_from_slice(&(total_len as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    frame.extend_from_slice(&crc32_ieee(&frame[..8]).to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(&crc32_ieee(&frame).to_be_bytes());
    frame
}

fn push_event_stream_string_header(out: &mut Vec<u8>, name: &str, value: &str) {
    out.push(name.len() as u8);
    out.extend_from_slice(name.as_bytes());
    out.push(7);
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn bedrock_env_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

struct ScopedEnv {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        if let Some(value) = self.previous.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn build_converse_body_maps_messages_tools_and_1m_beta() {
    let tools = vec![ToolSchema::new(
        "terminal",
        "Run commands",
        JsonSchema::new("object"),
    )];
    let body = build_converse_body(
        "global.anthropic.claude-opus-4-7",
        &[Message::system("system"), Message::user("hello")],
        &tools,
        Some(8192),
        Some(0.2),
        None,
    );
    assert_eq!(body["system"][0]["text"], "system");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["inferenceConfig"]["maxTokens"], 8192);
    assert_eq!(
        body["toolConfig"]["tools"][0]["toolSpec"]["name"],
        "terminal"
    );
    let betas = body["additionalModelRequestFields"]["anthropic_beta"]
        .as_array()
        .expect("anthropic betas");
    assert!(betas.iter().any(|v| v == CONTEXT_1M_BETA));
}

#[test]
fn build_converse_body_passes_top_p_guardrails_and_strips_unsupported_tools() {
    let tools = vec![ToolSchema::new("test", "Test", JsonSchema::new("object"))];
    let body = build_converse_body(
        "us.deepseek.r1-v1:0",
        &[Message::user("hello")],
        &tools,
        None,
        Some(0.7),
        Some(&json!({
            "top_p": 0.9,
            "guardrail_config": {
                "guardrailIdentifier": "gr-123",
                "guardrailVersion": "1"
            }
        })),
    );
    assert_eq!(body["inferenceConfig"]["temperature"], 0.7);
    assert_eq!(body["inferenceConfig"]["topP"], 0.9);
    assert_eq!(body["guardrailConfig"]["guardrailIdentifier"], "gr-123");
    assert!(body.get("toolConfig").is_none());
}

#[test]
fn convert_messages_merges_roles_and_enforces_user_boundaries() {
    let messages = vec![
        Message::user("first"),
        Message::user("second"),
        Message::assistant("part 1"),
        Message::assistant("part 2"),
    ];
    let (_system, converted) = convert_messages_to_bedrock(&messages);
    assert_eq!(converted.first().unwrap()["role"], "user");
    assert_eq!(converted.last().unwrap()["role"], "user");
    let user_messages = converted
        .iter()
        .filter(|message| message["role"] == "user")
        .count();
    let assistant_messages = converted
        .iter()
        .filter(|message| message["role"] == "assistant")
        .count();
    assert_eq!(user_messages, 2);
    assert_eq!(assistant_messages, 1);
    let assistant_text = converted[1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(assistant_text.contains(&"part 1"));
    assert!(assistant_text.contains(&"part 2"));
}

#[test]
fn convert_messages_decodes_acp_multimodal_data_url_and_empty_placeholder() {
    let parts = json!([
        {"type": "text", "text": "what is here"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}}
    ]);
    let marker = format!("{ACP_MULTIMODAL_PREFIX}{parts}");
    let (_system, converted) =
        convert_messages_to_bedrock(&[Message::user(marker), Message::user("   ")]);
    let blocks = converted[0]["content"].as_array().expect("content blocks");
    assert!(blocks.iter().any(|block| block["text"] == "what is here"));
    let image = blocks
        .iter()
        .find_map(|block| block.get("image"))
        .expect("image block");
    assert_eq!(image["format"], "png");
    assert_eq!(image["source"]["bytes"], "iVBORw0KGgo=");
    assert!(blocks.iter().any(|block| block["text"] == " "));
}

#[test]
fn convert_tool_schema_defaults_empty_parameters_to_object_schema() {
    let tools = vec![ToolSchema::new(
        "noop",
        "No-op",
        JsonSchema {
            schema_type: None,
            properties: None,
            required: None,
            additional_properties: None,
            defs: None,
        },
    )];
    let converted = convert_tools_to_bedrock(&tools);
    assert_eq!(
        converted[0]["toolSpec"]["inputSchema"]["json"],
        json!({"type": "object", "properties": {}})
    );
}

#[test]
fn parse_bedrock_response_preserves_text_tool_reasoning_and_usage() {
    let raw = json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [
                    {"reasoningContent": {"text": "Let me think..."}},
                    {"text": "Answer."},
                    {"toolUse": {
                        "toolUseId": "tool_1",
                        "name": "terminal",
                        "input": {"command": "ls"}
                    }}
                ]
            }
        },
        "stopReason": "tool_use",
        "usage": {"inputTokens": 10, "outputTokens": 5, "totalTokens": 15}
    });
    let response = parse_bedrock_response(&raw, "anthropic.claude").expect("response");
    assert_eq!(response.message.content.as_deref(), Some("Answer."));
    assert_eq!(
        response.message.reasoning_content.as_deref(),
        Some("Let me think...")
    );
    assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(response.usage.expect("usage").total_tokens, 15);
    let calls = response.message.tool_calls.expect("tool calls");
    assert_eq!(calls[0].function.name, "terminal");
    assert_eq!(calls[0].function.arguments, r#"{"command":"ls"}"#);
}

#[test]
fn parse_bedrock_response_handles_empty_content_and_tool_finish_override() {
    let empty = json!({
        "output": {"message": {"role": "assistant", "content": []}},
        "stopReason": "end_turn",
        "usage": {"inputTokens": 1, "outputTokens": 0}
    });
    let response = parse_bedrock_response(&empty, "anthropic.claude").expect("empty response");
    assert_eq!(response.message.content, None);
    assert_eq!(response.message.tool_calls, None);
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));

    let tool = json!({
        "output": {
            "message": {
                "role": "assistant",
                "content": [{"toolUse": {"toolUseId": "c1", "name": "search", "input": {}}}]
            }
        },
        "stopReason": "end_turn",
        "usage": {"inputTokens": 1, "outputTokens": 1}
    });
    let response = parse_bedrock_response(&tool, "anthropic.claude").expect("tool response");
    assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(
        response.message.tool_calls.unwrap()[0].function.arguments,
        "{}"
    );
}

#[test]
fn parse_bedrock_stream_events_collects_text_tool_reasoning_and_usage() {
    let raw = json!({
        "stream": [
            {"messageStart": {"role": "assistant"}},
            {"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"text": "Hello"}}},
            {"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"text": ", world"}}},
            {"contentBlockDelta": {"contentBlockIndex": 1, "delta": {
                "reasoningContent": {"text": "thinking"}
            }}},
            {"contentBlockStart": {"contentBlockIndex": 2, "start": {
                "toolUse": {"toolUseId": "call_1", "name": "read_file"}
            }}},
            {"contentBlockDelta": {"contentBlockIndex": 2, "delta": {
                "toolUse": {"input": "{\"path\":"}
            }}},
            {"contentBlockDelta": {"contentBlockIndex": 2, "delta": {
                "toolUse": {"input": "\"/tmp/f\"}"}
            }}},
            {"messageStop": {"stopReason": "end_turn"}},
            {"metadata": {"usage": {"inputTokens": 5, "outputTokens": 3}}}
        ]
    });
    let response = parse_bedrock_stream_events(&raw, "anthropic.claude").expect("stream");
    assert_eq!(response.message.content.as_deref(), Some("Hello, world"));
    assert_eq!(
        response.message.reasoning_content.as_deref(),
        Some("thinking")
    );
    assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(response.usage.expect("usage").total_tokens, 8);
    let call = &response.message.tool_calls.unwrap()[0];
    assert_eq!(call.id, "call_1");
    assert_eq!(call.function.name, "read_file");
    assert_eq!(call.function.arguments, r#"{"path":"/tmp/f"}"#);
}

#[test]
fn aws_event_stream_decoder_maps_bedrock_events_to_chunks() {
    let frames = [
        encode_event_stream_message(
            "contentBlockDelta",
            json!({"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"text": "Hello"}}}),
        ),
        encode_event_stream_message(
            "contentBlockDelta",
            json!({"contentBlockDelta": {"contentBlockIndex": 1, "delta": {
                "reasoningContent": {"text": "thinking"}
            }}}),
        ),
        encode_event_stream_message(
            "contentBlockStart",
            json!({"contentBlockStart": {"contentBlockIndex": 2, "start": {
                "toolUse": {"toolUseId": "tool_1", "name": "read_file"}
            }}}),
        ),
        encode_event_stream_message(
            "contentBlockDelta",
            json!({"contentBlockDelta": {"contentBlockIndex": 2, "delta": {
                "toolUse": {"input": "{\"path\":\"/tmp/f\"}"}
            }}}),
        ),
        encode_event_stream_message(
            "metadata",
            json!({"metadata": {"usage": {"inputTokens": 5, "outputTokens": 3}}}),
        ),
        encode_event_stream_message(
            "messageStop",
            json!({"messageStop": {"stopReason": "end_turn"}}),
        ),
    ];
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&frames[0][..frames[0].len() / 2]);
    assert!(take_aws_event_stream_message(&mut buffer)
        .expect("partial frame")
        .is_none());
    buffer.extend_from_slice(&frames[0][frames[0].len() / 2..]);
    for frame in frames.iter().skip(1) {
        buffer.extend_from_slice(frame);
    }

    let mut chunks = Vec::new();
    while let Some(message) =
        take_aws_event_stream_message(&mut buffer).expect("event stream frame")
    {
        let event = decode_bedrock_event_stream_message(&message)
            .expect("bedrock event")
            .expect("nonempty event");
        chunks.extend(bedrock_stream_event_to_chunks(&event).expect("chunks"));
    }

    assert!(buffer.is_empty());
    assert!(chunks.iter().any(|chunk| {
        chunk
            .delta
            .as_ref()
            .and_then(|delta| delta.content.as_deref())
            == Some("Hello")
    }));
    assert!(chunks.iter().any(|chunk| {
        chunk
            .delta
            .as_ref()
            .and_then(|delta| delta.extra.as_ref())
            .and_then(|extra| extra.get("thinking"))
            .and_then(Value::as_str)
            == Some("thinking")
    }));
    let tool_delta = chunks
        .iter()
        .filter_map(|chunk| chunk.delta.as_ref())
        .filter_map(|delta| delta.tool_calls.as_ref())
        .flat_map(|calls| calls.iter())
        .find(|call| call.function.as_ref().and_then(|f| f.name.as_deref()) == Some("read_file"))
        .expect("tool start delta");
    assert_eq!(tool_delta.index, 2);
    assert_eq!(tool_delta.id.as_deref(), Some("tool_1"));
    assert!(chunks.iter().any(|chunk| {
        chunk
            .delta
            .as_ref()
            .and_then(|delta| delta.tool_calls.as_ref())
            .and_then(|calls| calls.first())
            .and_then(|call| call.function.as_ref())
            .and_then(|function| function.arguments.as_deref())
            == Some(r#"{"path":"/tmp/f"}"#)
    }));
    assert_eq!(
        chunks
            .iter()
            .find_map(|chunk| chunk.usage.as_ref())
            .unwrap()
            .total_tokens,
        8
    );
    assert_eq!(
        chunks
            .iter()
            .find_map(|chunk| chunk.finish_reason.as_deref()),
        Some("stop")
    );
}

#[test]
fn aws_event_stream_decoder_rejects_bad_crc() {
    let mut frame = encode_event_stream_message("metadata", json!({"metadata": {"usage": {}}}));
    let last = frame.len() - 1;
    frame[last] ^= 0xff;
    let mut buffer = frame;
    let err = take_aws_event_stream_message(&mut buffer).expect_err("CRC failure");
    assert!(matches!(err, AgentError::LlmApi(message) if message.contains("checksum")));
}

#[test]
fn crc32_ieee_matches_standard_check_value() {
    assert_eq!(crc32_ieee(b"123456789"), 0xcbf4_3926);
}

#[test]
fn aws_event_stream_decoder_maps_payload_exceptions() {
    let mut buffer = encode_event_stream_message(
        "validationException",
        json!({"validationException": {"message": "bad input"}}),
    );
    let message = take_aws_event_stream_message(&mut buffer)
        .expect("event frame")
        .expect("complete frame");
    let err = decode_bedrock_event_stream_message(&message).expect_err("stream exception error");
    assert!(matches!(err, AgentError::LlmApi(message) if message.contains("400")));
}

#[tokio::test]
async fn bedrock_chat_completion_stream_uses_converse_stream_transport() {
    let _lock = bedrock_env_lock().lock().await;
    let _token = ScopedEnv::set("AWS_BEARER_TOKEN_BEDROCK", "test-token");
    let body = [
        encode_event_stream_message(
            "contentBlockStart",
            json!({"contentBlockStart": {"contentBlockIndex": 0, "start": {
                "toolUse": {"toolUseId": "call_1", "name": "read_file"}
            }}}),
        ),
        encode_event_stream_message(
            "contentBlockDelta",
            json!({"contentBlockDelta": {"contentBlockIndex": 0, "delta": {
                "toolUse": {"input": "{\"path\":\"/tmp/f\"}"}
            }}}),
        ),
        encode_event_stream_message(
            "messageStop",
            json!({"messageStop": {"stopReason": "end_turn"}}),
        ),
        encode_event_stream_message(
            "metadata",
            json!({"metadata": {"usage": {"inputTokens": 2, "outputTokens": 4}}}),
        ),
    ]
    .concat();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock Bedrock");
    let addr = listener.local_addr().expect("mock address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept request");
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = socket.read(&mut buf).await.expect("read request");
            assert!(n > 0, "client closed before headers");
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let header_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("request headers")
            + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
        let content_len = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        while request.len().saturating_sub(header_end) < content_len {
            let n = socket.read(&mut buf).await.expect("read request body");
            assert!(n > 0, "client closed before body");
            request.extend_from_slice(&buf[..n]);
        }
        assert!(
            headers.starts_with("POST /model/anthropic.claude/converse-stream HTTP/1.1"),
            "unexpected request line: {headers}"
        );
        assert!(
            headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer test-token")),
            "missing bearer authorization: {headers}"
        );
        let response_headers = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/vnd.amazon.eventstream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(response_headers.as_bytes())
            .await
            .expect("write response headers");
        socket.write_all(&body).await.expect("write response body");
    });

    let provider = BedrockProvider::new()
        .with_region("us-east-1")
        .with_model("anthropic.claude")
        .with_base_url(format!("http://{addr}"));
    let chunks = provider
        .chat_completion_stream(&[Message::user("hello")], &[], None, None, None, None)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("stream chunks");
    server.await.expect("mock server");

    assert!(chunks.iter().any(|chunk| {
        chunk
            .delta
            .as_ref()
            .and_then(|delta| delta.tool_calls.as_ref())
            .and_then(|calls| calls.first())
            .and_then(|call| call.function.as_ref())
            .and_then(|function| function.name.as_deref())
            == Some("read_file")
    }));
    assert!(chunks.iter().any(|chunk| {
        chunk
            .delta
            .as_ref()
            .and_then(|delta| delta.tool_calls.as_ref())
            .and_then(|calls| calls.first())
            .and_then(|call| call.function.as_ref())
            .and_then(|function| function.arguments.as_deref())
            == Some(r#"{"path":"/tmp/f"}"#)
    }));
    assert_eq!(
        chunks
            .iter()
            .find_map(|chunk| chunk.finish_reason.as_deref()),
        Some("tool_calls")
    );
    assert_eq!(
        chunks
            .iter()
            .find_map(|chunk| chunk.usage.as_ref())
            .unwrap()
            .total_tokens,
        6
    );
}

#[test]
fn finish_reason_mapping_matches_bedrock_transport_contract() {
    assert_eq!(
        map_bedrock_finish_reason(Some("end_turn")).as_deref(),
        Some("stop")
    );
    assert_eq!(
        map_bedrock_finish_reason(Some("stop_sequence")).as_deref(),
        Some("stop")
    );
    assert_eq!(
        map_bedrock_finish_reason(Some("tool_use")).as_deref(),
        Some("tool_calls")
    );
    assert_eq!(
        map_bedrock_finish_reason(Some("max_tokens")).as_deref(),
        Some("length")
    );
    assert_eq!(
        map_bedrock_finish_reason(Some("guardrail_intervened")).as_deref(),
        Some("content_filter")
    );
    assert_eq!(
        map_bedrock_finish_reason(Some("content_filtered")).as_deref(),
        Some("content_filter")
    );
    assert_eq!(
        map_bedrock_finish_reason(Some("unknown")).as_deref(),
        Some("stop")
    );
}

#[test]
fn catalog_parser_accepts_foundation_models_and_inference_profiles() {
    let raw = json!({
        "modelSummaries": [
            {"modelId": "anthropic.claude-3-5-sonnet-20241022-v2:0"}
        ],
        "inferenceProfileSummaries": [
            {"inferenceProfileId": "eu.anthropic.claude-sonnet-4-6"}
        ]
    });
    let ids = parse_bedrock_catalog_model_ids(&raw);
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().any(|id| id.starts_with("eu.anthropic.")));
}

#[test]
fn catalog_parser_filters_unsupported_models_and_sorts_global_profiles_first() {
    let raw = json!({
        "modelSummaries": [
            {
                "modelId": "old-model",
                "outputModalities": ["TEXT"],
                "responseStreamingSupported": true,
                "modelLifecycle": {"status": "LEGACY"}
            },
            {
                "modelId": "embed-model",
                "outputModalities": ["EMBEDDING"],
                "responseStreamingSupported": false,
                "modelLifecycle": {"status": "ACTIVE"}
            },
            {
                "modelId": "anthropic.claude-v2",
                "outputModalities": ["TEXT"],
                "responseStreamingSupported": true,
                "modelLifecycle": {"status": "ACTIVE"}
            }
        ],
        "inferenceProfileSummaries": [
            {"inferenceProfileId": "us.anthropic.claude-v2", "status": "ACTIVE"},
            {"inferenceProfileId": "global.anthropic.claude-v2", "status": "ACTIVE"}
        ]
    });
    let ids = parse_bedrock_catalog_model_ids(&raw);
    assert_eq!(
        ids.first().map(String::as_str),
        Some("global.anthropic.claude-v2")
    );
    assert!(ids.iter().any(|id| id == "anthropic.claude-v2"));
    assert!(!ids.iter().any(|id| id == "old-model"));
    assert!(!ids.iter().any(|id| id == "embed-model"));
}

#[test]
fn bedrock_context_tool_support_and_error_helpers_match_adapter_policy() {
    assert_eq!(
        get_bedrock_context_length("us.anthropic.claude-sonnet-4-6"),
        200_000
    );
    assert_eq!(get_bedrock_context_length("amazon.nova-pro-v1:0"), 300_000);
    assert_eq!(
        get_bedrock_context_length("amazon.nova-micro-v1:0"),
        128_000
    );
    assert_eq!(
        get_bedrock_context_length("unknown.model-v1:0"),
        BEDROCK_DEFAULT_CONTEXT_LENGTH
    );
    assert!(model_supports_bedrock_tool_use(
        "us.anthropic.claude-sonnet-4-6"
    ));
    assert!(model_supports_bedrock_tool_use("deepseek.v3.2"));
    assert!(!model_supports_bedrock_tool_use("us.deepseek.r1-v1:0"));
    assert!(!model_supports_bedrock_tool_use(
        "stability.stable-diffusion-xl"
    ));
    assert!(!model_supports_bedrock_tool_use("cohere.embed-v4"));
    assert_eq!(
        classify_bedrock_error("ValidationException: input is too long").as_str(),
        "context_overflow"
    );
    assert_eq!(
        classify_bedrock_error("Too many concurrent requests").as_str(),
        "rate_limit"
    );
    assert_eq!(
        classify_bedrock_error("ModelTimeoutException").as_str(),
        "overloaded"
    );
    assert_eq!(
        classify_bedrock_error("SomeRandomError").as_str(),
        "unknown"
    );
}

#[test]
fn anthropic_detector_accepts_regional_inference_profile_prefixes() {
    assert!(is_bedrock_anthropic_model_id(
        "au.anthropic.claude-sonnet-4-6"
    ));
    assert!(is_bedrock_anthropic_model_id(
        "jp.anthropic.claude-sonnet-4-6"
    ));
    assert!(is_bedrock_anthropic_model_id(
        "apac.anthropic.claude-sonnet-4-6"
    ));
    assert!(!is_bedrock_anthropic_model_id("us.amazon.nova-pro-v1:0"));
}

#[test]
fn sigv4_headers_include_required_bedrock_fields() {
    let creds = AwsCredentials {
        access_key_id: "AKIDEXAMPLE".to_string(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
        session_token: Some("session".to_string()),
    };
    let auth = BedrockAuth::SigV4(creds);
    let headers = bedrock_request_headers(
        BedrockHeaderRequest {
            method: "POST",
            url: "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude%3A0/converse",
            region: "us-east-1",
            service: "bedrock",
            body: br#"{"messages":[]}"#,
            anthropic_beta: Some(CONTEXT_1M_BETA),
            now: DateTime::parse_from_rfc3339("2026-05-30T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        },
        &auth,
    )
    .expect("headers");
    assert_eq!(
        headers.get("x-amz-date").map(String::as_str),
        Some("20260530T000000Z")
    );
    assert_eq!(
        headers.get("x-amz-security-token").map(String::as_str),
        Some("session")
    );
    assert!(headers.get("authorization").expect("auth").starts_with(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260530/us-east-1/bedrock/aws4_request"
    ));
    assert_eq!(
        headers.get("anthropic-beta").map(String::as_str),
        Some(CONTEXT_1M_BETA)
    );
}
