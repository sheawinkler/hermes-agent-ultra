pub fn validate_bedrock_response(value: &Value) -> bool {
    value.get("output").and_then(|v| v.get("message")).is_some() && value.get("error").is_none()
}

pub fn map_bedrock_finish_reason(reason: Option<&str>) -> Option<String> {
    Some(
        match reason.unwrap_or("end_turn") {
            "end_turn" | "stop_sequence" => "stop",
            "tool_use" => "tool_calls",
            "max_tokens" => "length",
            "content_filtered" | "guardrail_intervened" => "content_filter",
            _ => "stop",
        }
        .to_string(),
    )
}

pub fn parse_bedrock_response(json: &Value, model: &str) -> Result<LlmResponse, AgentError> {
    if let Some(response) = parse_openai_like_response(json, model) {
        return Ok(response);
    }
    if !validate_bedrock_response(json) {
        return Err(AgentError::LlmApi(format!(
            "Invalid Bedrock response shape: {}",
            truncate_json(json, 600)
        )));
    }
    let content_blocks = json
        .get("output")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.get("content"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut tool_calls = Vec::new();
    for block in content_blocks {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                text_parts.push(text.to_string());
            }
        }
        if let Some(text) = block
            .get("reasoningContent")
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
        {
            if !text.is_empty() {
                reasoning_parts.push(text.to_string());
            }
        }
        if let Some(tool_use) = block.get("toolUse") {
            let id = tool_use
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = tool_use
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let arguments = tool_use
                .get("input")
                .cloned()
                .unwrap_or_else(|| json!({}))
                .to_string();
            tool_calls.push(ToolCall {
                id,
                function: FunctionCall { name, arguments },
                extra_content: None,
            });
        }
    }
    let usage = json.get("usage").map(|usage| UsageStats {
        prompt_tokens: usage
            .get("inputTokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        completion_tokens: usage
            .get("outputTokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        total_tokens: usage
            .get("totalTokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                usage
                    .get("inputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    + usage
                        .get("outputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default()
            }),
        estimated_cost: None,
    });
    let finish_reason = if tool_calls.is_empty() {
        map_bedrock_finish_reason(json.get("stopReason").and_then(Value::as_str))
    } else {
        Some("tool_calls".to_string())
    };
    Ok(LlmResponse {
        message: Message {
            role: MessageRole::Assistant,
            content: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
            reasoning_content: if reasoning_parts.is_empty() {
                None
            } else {
                Some(reasoning_parts.join("\n"))
            },
            anthropic_content_blocks: None,
            cache_control: None,
        },
        usage,
        model: model.to_string(),
        finish_reason,
    })
}

pub fn parse_bedrock_stream_events(json: &Value, model: &str) -> Result<LlmResponse, AgentError> {
    let events = json
        .get("stream")
        .and_then(Value::as_array)
        .or_else(|| json.as_array())
        .ok_or_else(|| {
            AgentError::LlmApi(format!(
                "Invalid Bedrock ConverseStream shape: {}",
                truncate_json(json, 600)
            ))
        })?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools: BTreeMap<u64, StreamToolAccumulator> = BTreeMap::new();
    let mut stop_reason: Option<String> = None;
    let mut usage: Option<UsageStats> = None;

    for event in events {
        if let Some(start) = event.get("contentBlockStart") {
            let index = stream_content_block_index(start);
            if let Some(tool_use) = start.get("start").and_then(|v| v.get("toolUse")) {
                let entry = tools.entry(index).or_default();
                entry.id = tool_use
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                entry.name = tool_use
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        if let Some(delta_event) = event.get("contentBlockDelta") {
            let index = stream_content_block_index(delta_event);
            if let Some(delta) = delta_event.get("delta") {
                if let Some(fragment) = delta.get("text").and_then(Value::as_str) {
                    text.push_str(fragment);
                }
                if let Some(fragment) = delta
                    .get("reasoningContent")
                    .and_then(|v| v.get("text"))
                    .and_then(Value::as_str)
                {
                    reasoning.push_str(fragment);
                }
                if let Some(tool_use) = delta.get("toolUse") {
                    let entry = tools.entry(index).or_default();
                    if let Some(id) = tool_use.get("toolUseId").and_then(Value::as_str) {
                        entry.id = Some(id.to_string());
                    }
                    if let Some(name) = tool_use.get("name").and_then(Value::as_str) {
                        entry.name = Some(name.to_string());
                    }
                    if let Some(input) = tool_use.get("input").and_then(Value::as_str) {
                        entry.input_fragments.push_str(input);
                    } else if let Some(input) = tool_use.get("input") {
                        entry.input_fragments.push_str(&input.to_string());
                    }
                }
            }
        }
        if let Some(message_stop) = event.get("messageStop") {
            stop_reason = message_stop
                .get("stopReason")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if let Some(metadata) = event.get("metadata") {
            if let Some(raw_usage) = metadata.get("usage") {
                usage = Some(parse_bedrock_usage(raw_usage));
            }
        }
    }

    let tool_calls = tools
        .into_values()
        .filter_map(|tool| {
            let name = tool.name?;
            if name.trim().is_empty() {
                return None;
            }
            Some(ToolCall {
                id: tool.id.unwrap_or_default(),
                function: FunctionCall {
                    name,
                    arguments: normalize_tool_input_arguments(&tool.input_fragments),
                },
                extra_content: None,
            })
        })
        .collect::<Vec<_>>();
    let finish_reason = if tool_calls.is_empty() {
        map_bedrock_finish_reason(stop_reason.as_deref())
    } else {
        Some("tool_calls".to_string())
    };
    Ok(LlmResponse {
        message: Message {
            role: MessageRole::Assistant,
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
            reasoning_content: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            anthropic_content_blocks: None,
            cache_control: None,
        },
        usage,
        model: model.to_string(),
        finish_reason,
    })
}

fn take_aws_event_stream_message(
    buffer: &mut Vec<u8>,
) -> Result<Option<AwsEventStreamMessage>, AgentError> {
    if buffer.len() < 12 {
        return Ok(None);
    }
    let total_len = read_be_u32(&buffer[0..4]) as usize;
    let headers_len = read_be_u32(&buffer[4..8]) as usize;
    if total_len < 16 {
        return Err(AgentError::LlmApi(format!(
            "Invalid Bedrock event stream frame length: {total_len}"
        )));
    }
    if total_len > buffer.len() {
        return Ok(None);
    }
    if headers_len > total_len.saturating_sub(16) {
        return Err(AgentError::LlmApi(format!(
            "Invalid Bedrock event stream headers length: {headers_len}"
        )));
    }

    let frame: Vec<u8> = buffer.drain(..total_len).collect();
    let expected_prelude_crc = read_be_u32(&frame[8..12]);
    let actual_prelude_crc = crc32_ieee(&frame[..8]);
    if expected_prelude_crc != actual_prelude_crc {
        return Err(AgentError::LlmApi(
            "Invalid Bedrock event stream prelude checksum".to_string(),
        ));
    }

    let expected_message_crc = read_be_u32(&frame[total_len - 4..total_len]);
    let actual_message_crc = crc32_ieee(&frame[..total_len - 4]);
    if expected_message_crc != actual_message_crc {
        return Err(AgentError::LlmApi(
            "Invalid Bedrock event stream message checksum".to_string(),
        ));
    }

    let headers_start = 12;
    let headers_end = headers_start + headers_len;
    let payload_end = total_len - 4;
    Ok(Some(AwsEventStreamMessage {
        headers: parse_aws_event_stream_headers(&frame[headers_start..headers_end])?,
        payload: frame[headers_end..payload_end].to_vec(),
    }))
}

fn decode_bedrock_event_stream_message(
    message: &AwsEventStreamMessage,
) -> Result<Option<Value>, AgentError> {
    if message.payload.is_empty() {
        return Ok(None);
    }
    let payload: Value = serde_json::from_slice(&message.payload).map_err(|err| {
        AgentError::LlmApi(format!("Bedrock event stream JSON parse failed: {err}"))
    })?;
    let message_type = message.headers.get(":message-type").map(String::as_str);
    let event_type = message.headers.get(":event-type").map(String::as_str);
    if matches!(message_type, Some("exception"))
        || event_type.is_some_and(|event| event.ends_with("Exception"))
        || bedrock_stream_exception_status(event_type).is_some()
        || bedrock_stream_payload_exception_status(&payload).is_some()
    {
        return Err(map_bedrock_error(
            bedrock_stream_exception_status(event_type)
                .or_else(|| bedrock_stream_payload_exception_status(&payload))
                .unwrap_or(500),
            &payload.to_string(),
        ));
    }
    if is_bedrock_stream_event_value(&payload) {
        return Ok(Some(payload));
    }
    if let Some(event_type) = event_type.filter(|event| !event.is_empty()) {
        return Ok(Some(json!({ event_type: payload })));
    }
    Ok(Some(payload))
}

fn bedrock_stream_event_to_chunks(event: &Value) -> Result<Vec<StreamChunk>, AgentError> {
    let mut chunks = Vec::new();
    if let Some(start_event) = event.get("contentBlockStart") {
        if let Some(tool_use) = start_event
            .get("start")
            .and_then(|start| start.get("toolUse"))
        {
            let index = stream_content_block_index(start_event) as u32;
            let id = tool_use
                .get("toolUseId")
                .and_then(Value::as_str)
                .map(str::to_string);
            let name = tool_use
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string);
            chunks.push(StreamChunk {
                delta: Some(StreamDelta {
                    content: None,
                    tool_calls: Some(vec![ToolCallDelta {
                        index,
                        id,
                        function: Some(FunctionCallDelta {
                            name,
                            arguments: None,
                        }),
                    }]),
                    extra: None,
                }),
                finish_reason: None,
                usage: None,
            });
        }
    }

    if let Some(delta_event) = event.get("contentBlockDelta") {
        if let Some(delta) = delta_event.get("delta") {
            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                chunks.push(StreamChunk {
                    delta: Some(StreamDelta {
                        content: Some(text.to_string()),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
            if let Some(reasoning) = delta
                .get("reasoningContent")
                .and_then(|value| value.get("text"))
                .and_then(Value::as_str)
            {
                chunks.push(StreamChunk {
                    delta: Some(StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(json!({ "thinking": reasoning })),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
            if let Some(tool_use) = delta.get("toolUse") {
                let index = stream_content_block_index(delta_event) as u32;
                let id = tool_use
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let name = tool_use
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let arguments = tool_use.get("input").map(|input| {
                    input
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| input.to_string())
                });
                chunks.push(StreamChunk {
                    delta: Some(StreamDelta {
                        content: None,
                        tool_calls: Some(vec![ToolCallDelta {
                            index,
                            id,
                            function: Some(FunctionCallDelta { name, arguments }),
                        }]),
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
        }
    }

    if let Some(stop_event) = event.get("messageStop") {
        chunks.push(StreamChunk {
            delta: None,
            finish_reason: map_bedrock_finish_reason(
                stop_event.get("stopReason").and_then(Value::as_str),
            ),
            usage: None,
        });
    }

    if let Some(metadata) = event.get("metadata") {
        if let Some(raw_usage) = metadata.get("usage") {
            chunks.push(StreamChunk {
                delta: None,
                finish_reason: None,
                usage: Some(parse_bedrock_usage(raw_usage)),
            });
        }
    }

    Ok(chunks)
}

fn is_bedrock_stream_event_value(value: &Value) -> bool {
    [
        "messageStart",
        "contentBlockStart",
        "contentBlockDelta",
        "contentBlockStop",
        "messageStop",
        "metadata",
        "internalServerException",
        "modelStreamErrorException",
        "serviceUnavailableException",
        "throttlingException",
        "validationException",
    ]
    .iter()
    .any(|key| value.get(*key).is_some())
}

fn bedrock_stream_exception_status(event_type: Option<&str>) -> Option<u16> {
    match event_type? {
        "validationException" => Some(400),
        "throttlingException" => Some(429),
        "modelTimeoutException" => Some(408),
        "modelStreamErrorException" => Some(424),
        "serviceUnavailableException" => Some(503),
        "internalServerException" => Some(500),
        _ => None,
    }
}

fn bedrock_stream_payload_exception_status(payload: &Value) -> Option<u16> {
    [
        ("validationException", 400),
        ("throttlingException", 429),
        ("modelTimeoutException", 408),
        ("modelStreamErrorException", 424),
        ("serviceUnavailableException", 503),
        ("internalServerException", 500),
    ]
    .iter()
    .find_map(|(key, status)| payload.get(*key).map(|_| *status))
}

fn parse_aws_event_stream_headers(raw: &[u8]) -> Result<HashMap<String, String>, AgentError> {
    let mut headers = HashMap::new();
    let mut offset = 0;
    while offset < raw.len() {
        let name_len = *raw.get(offset).ok_or_else(|| {
            AgentError::LlmApi("Malformed Bedrock event stream header name".to_string())
        })? as usize;
        offset += 1;
        if name_len == 0 || offset + name_len > raw.len() {
            return Err(AgentError::LlmApi(
                "Malformed Bedrock event stream header name".to_string(),
            ));
        }
        let name = std::str::from_utf8(&raw[offset..offset + name_len])
            .map_err(|err| {
                AgentError::LlmApi(format!("Bedrock event stream header name UTF-8: {err}"))
            })?
            .to_string();
        offset += name_len;
        let value_type = *raw.get(offset).ok_or_else(|| {
            AgentError::LlmApi("Malformed Bedrock event stream header value".to_string())
        })?;
        offset += 1;
        match value_type {
            0 => {
                headers.insert(name, "true".to_string());
            }
            1 => {
                headers.insert(name, "false".to_string());
            }
            2 => {
                ensure_header_bytes(raw, offset, 1)?;
                headers.insert(name, i8::from_be_bytes([raw[offset]]).to_string());
                offset += 1;
            }
            3 => {
                ensure_header_bytes(raw, offset, 2)?;
                headers.insert(
                    name,
                    i16::from_be_bytes([raw[offset], raw[offset + 1]]).to_string(),
                );
                offset += 2;
            }
            4 => {
                ensure_header_bytes(raw, offset, 4)?;
                headers.insert(
                    name,
                    i32::from_be_bytes([
                        raw[offset],
                        raw[offset + 1],
                        raw[offset + 2],
                        raw[offset + 3],
                    ])
                    .to_string(),
                );
                offset += 4;
            }
            5 | 8 => {
                ensure_header_bytes(raw, offset, 8)?;
                headers.insert(
                    name,
                    i64::from_be_bytes([
                        raw[offset],
                        raw[offset + 1],
                        raw[offset + 2],
                        raw[offset + 3],
                        raw[offset + 4],
                        raw[offset + 5],
                        raw[offset + 6],
                        raw[offset + 7],
                    ])
                    .to_string(),
                );
                offset += 8;
            }
            6 => {
                let len = read_header_len(raw, &mut offset)?;
                ensure_header_bytes(raw, offset, len)?;
                headers.insert(name, hex::encode(&raw[offset..offset + len]));
                offset += len;
            }
            7 => {
                let len = read_header_len(raw, &mut offset)?;
                ensure_header_bytes(raw, offset, len)?;
                let value = std::str::from_utf8(&raw[offset..offset + len])
                    .map_err(|err| {
                        AgentError::LlmApi(format!("Bedrock event stream header UTF-8: {err}"))
                    })?
                    .to_string();
                headers.insert(name, value);
                offset += len;
            }
            9 => {
                ensure_header_bytes(raw, offset, 16)?;
                headers.insert(name, hex::encode(&raw[offset..offset + 16]));
                offset += 16;
            }
            other => {
                return Err(AgentError::LlmApi(format!(
                    "Unsupported Bedrock event stream header value type: {other}"
                )));
            }
        }
    }
    Ok(headers)
}

fn read_header_len(raw: &[u8], offset: &mut usize) -> Result<usize, AgentError> {
    ensure_header_bytes(raw, *offset, 2)?;
    let len = u16::from_be_bytes([raw[*offset], raw[*offset + 1]]) as usize;
    *offset += 2;
    Ok(len)
}

fn ensure_header_bytes(raw: &[u8], offset: usize, len: usize) -> Result<(), AgentError> {
    if offset + len > raw.len() {
        return Err(AgentError::LlmApi(
            "Malformed Bedrock event stream header value".to_string(),
        ));
    }
    Ok(())
}

fn read_be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn stream_content_block_index(event: &Value) -> u64 {
    event
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn parse_bedrock_usage(usage: &Value) -> UsageStats {
    let prompt_tokens = usage
        .get("inputTokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("outputTokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let total_tokens = usage
        .get("totalTokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt_tokens + completion_tokens);
    UsageStats {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost: None,
    }
}

fn normalize_tool_input_arguments(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    serde_json::from_str::<Value>(trimmed)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

fn parse_openai_like_response(json: &Value, fallback_model: &str) -> Option<LlmResponse> {
    let choices = json.get("choices")?.as_array()?;
    let choice = choices.first()?;
    let message_obj = choice.get("message")?;
    let content = message_obj
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_calls = message_obj
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let function = tc.get("function")?;
                    Some(ToolCall {
                        id: tc
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        function: FunctionCall {
                            name: function.get("name")?.as_str()?.to_string(),
                            arguments: function
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}")
                                .to_string(),
                        },
                        extra_content: None,
                    })
                })
                .collect::<Vec<_>>()
        })
        .filter(|calls| !calls.is_empty());
    let usage = json.get("usage").map(|usage| UsageStats {
        prompt_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        estimated_cost: None,
    });
    Some(LlmResponse {
        message: Message {
            role: MessageRole::Assistant,
            content: Some(content),
            tool_calls,
            tool_call_id: None,
            name: None,
            reasoning_content: message_obj
                .get("reasoning")
                .or_else(|| message_obj.get("reasoning_content"))
                .and_then(Value::as_str)
                .map(str::to_string),
            anthropic_content_blocks: None,
            cache_control: None,
        },
        usage,
        model: json
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model)
            .to_string(),
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}
