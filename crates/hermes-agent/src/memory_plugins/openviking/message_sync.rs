fn openviking_sync_trace_enabled() -> bool {
    std::env::var(SYNC_TRACE_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn preview_sync_value(value: impl AsRef<str>) -> String {
    let mut text = value.as_ref().replace('\n', "\\n");
    if text.len() > 160 {
        text.truncate(160);
        text.push_str("...");
    }
    text
}

fn is_openviking_recall_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name.trim().to_ascii_lowercase().as_str(),
        VIKING_SEARCH_TOOL | VIKING_READ_TOOL | VIKING_BROWSE_TOOL
    )
}

fn value_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(key))
}

fn text_from_part(part: &Value) -> String {
    match part {
        Value::String(text) => text.clone(),
        Value::Object(_) => {
            let part_type = value_field(part, "type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if matches!(
                part_type.as_str(),
                "image" | "image_url" | "input_image" | "audio" | "input_audio"
            ) {
                return String::new();
            }
            if let Some(text) = [
                "text",
                "content",
                "input_text",
                "output_text",
                "summary_text",
            ]
            .iter()
            .find_map(|key| {
                value_field(part, key)
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            }) {
                text
            } else if part_type.is_empty() {
                part.to_string()
            } else {
                String::new()
            }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn message_text_from_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(text_from_part)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(_)) => text_from_part(content.expect("object content present")),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn message_text(message: &Value) -> String {
    message_text_from_content(value_field(message, "content"))
}

fn message_matches_text(message: &Value, expected: &str) -> bool {
    !expected.trim().is_empty() && message_text(message).trim() == expected.trim()
}

fn extract_current_turn_messages(
    messages: &[Value],
    user_content: &str,
    assistant_content: &str,
) -> Vec<Value> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut end_idx = None;
    if !assistant_content.trim().is_empty() {
        for (idx, message) in messages.iter().enumerate().rev() {
            if message.get("role").and_then(Value::as_str) == Some("assistant")
                && message_matches_text(message, assistant_content)
            {
                end_idx = Some(idx);
                break;
            }
        }
    }
    if end_idx.is_none() {
        for (idx, message) in messages.iter().enumerate().rev() {
            if message.get("role").and_then(Value::as_str) == Some("assistant") {
                end_idx = Some(idx);
                break;
            }
        }
    }
    let mut end_idx = end_idx.unwrap_or_else(|| messages.len().saturating_sub(1));
    while end_idx + 1 < messages.len()
        && messages[end_idx + 1].get("role").and_then(Value::as_str) == Some("tool")
    {
        end_idx += 1;
    }

    let mut start_idx = None;
    if !user_content.trim().is_empty() {
        for idx in (0..=end_idx).rev() {
            let message = &messages[idx];
            if message.get("role").and_then(Value::as_str) == Some("user")
                && message_matches_text(message, user_content)
            {
                start_idx = Some(idx);
                break;
            }
        }
    }
    if start_idx.is_none() {
        for idx in (0..=end_idx).rev() {
            if messages[idx].get("role").and_then(Value::as_str) == Some("user") {
                start_idx = Some(idx);
                break;
            }
        }
    }
    let Some(start_idx) = start_idx else {
        return Vec::new();
    };
    messages[start_idx..=end_idx].to_vec()
}

fn tool_call_id(tool_call: &Value) -> String {
    tool_call
        .get("id")
        .or_else(|| tool_call.get("tool_call_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tool_call_name(tool_call: &Value) -> String {
    tool_call
        .get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool_call.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tool_call_input(tool_call: &Value) -> Value {
    let raw_args = tool_call
        .get("function")
        .and_then(|function| function.get("arguments"))
        .or_else(|| tool_call.get("arguments"))
        .or_else(|| tool_call.get("args"));
    match raw_args {
        Some(Value::Object(_)) => raw_args.cloned().unwrap_or_else(|| json!({})),
        Some(Value::String(raw)) => {
            let raw = raw.trim();
            if raw.is_empty() {
                json!({})
            } else {
                match serde_json::from_str::<Value>(raw) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    Ok(parsed) => json!({"value": parsed}),
                    Err(_) => json!({"value": raw}),
                }
            }
        }
        Some(Value::Null) | None => json!({}),
        Some(other) => json!({"value": other}),
    }
}

fn tool_result_status(message: &Value) -> &'static str {
    let raw_status = message
        .get("status")
        .or_else(|| message.get("tool_status"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if TOOL_STATUS_ERROR_ALIASES.contains(&raw_status.as_str()) {
        return TOOL_STATUS_ERROR;
    }
    if TOOL_STATUS_COMPLETED_ALIASES.contains(&raw_status.as_str()) {
        return TOOL_STATUS_COMPLETED;
    }

    let text = message_text(message);
    if !text.trim().is_empty() {
        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
            let exit_code = parsed.get("exit_code").and_then(Value::as_i64);
            if parsed
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || parsed
                    .get("success")
                    .and_then(Value::as_bool)
                    .is_some_and(|success| !success)
                || parsed.get("error").is_some_and(|error| !error.is_null())
                || exit_code.is_some_and(|code| code != 0)
            {
                return TOOL_STATUS_ERROR;
            }
        }
    }
    TOOL_STATUS_COMPLETED
}

fn payload_message(role: &str, parts: Vec<Value>, assistant_peer_id: Option<&str>) -> Value {
    let mut payload = json!({"role": role, "parts": parts});
    if role == "assistant" {
        if let Some(peer_id) = assistant_peer_id {
            if !peer_id.trim().is_empty() {
                payload["peer_id"] = json!(peer_id);
            }
        }
    }
    payload
}

fn messages_to_openviking_batch(messages: &[Value], assistant_peer_id: Option<&str>) -> Vec<Value> {
    let mut tool_calls_by_id: HashMap<String, (String, Value)> = HashMap::new();
    let mut completed_tool_ids: HashSet<String> = HashSet::new();
    let mut skipped_tool_ids: HashSet<String> = HashSet::new();

    for message in messages {
        match message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "tool" => {
                let tool_id = message
                    .get("tool_call_id")
                    .or_else(|| message.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !tool_id.is_empty() {
                    completed_tool_ids.insert(tool_id.clone());
                    if message
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(is_openviking_recall_tool_name)
                    {
                        skipped_tool_ids.insert(tool_id);
                    }
                }
            }
            "assistant" => {
                for tool_call in message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if !tool_call.is_object() {
                        continue;
                    }
                    let tool_id = tool_call_id(tool_call);
                    let tool_name = tool_call_name(tool_call);
                    if !tool_id.is_empty() {
                        tool_calls_by_id.insert(
                            tool_id.clone(),
                            (tool_name.clone(), tool_call_input(tool_call)),
                        );
                        if is_openviking_recall_tool_name(&tool_name) {
                            skipped_tool_ids.insert(tool_id);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut payload_messages = Vec::new();
    let mut pending_tool_parts = Vec::new();
    let flush_tool_parts = |payload_messages: &mut Vec<Value>,
                            pending_tool_parts: &mut Vec<Value>| {
        if !pending_tool_parts.is_empty() {
            payload_messages.push(payload_message(
                "assistant",
                std::mem::take(pending_tool_parts),
                assistant_peer_id,
            ));
        }
    };

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(role, "system" | "developer") {
            continue;
        }

        if role == "tool" {
            let tool_id = message
                .get("tool_call_id")
                .or_else(|| message.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let prior_call = tool_calls_by_id.get(&tool_id);
            let tool_name = message
                .get("name")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| prior_call.map(|(name, _)| name.clone()))
                .unwrap_or_default();
            if skipped_tool_ids.contains(&tool_id) || is_openviking_recall_tool_name(&tool_name) {
                continue;
            }
            let tool_input = prior_call
                .map(|(_, input)| input.clone())
                .unwrap_or_else(|| json!({}));
            pending_tool_parts.push(json!({
                "type": "tool",
                "tool_id": tool_id,
                "tool_name": tool_name,
                "tool_input": tool_input,
                "tool_output": message_text(message),
                "tool_status": tool_result_status(message),
            }));
            continue;
        }

        if !matches!(role, "user" | "assistant") {
            continue;
        }

        flush_tool_parts(&mut payload_messages, &mut pending_tool_parts);
        let mut parts = Vec::new();
        let text = message_text(message);
        if !text.is_empty() {
            parts.push(json!({"type": "text", "text": text}));
        }

        if role == "assistant" {
            for tool_call in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if !tool_call.is_object() {
                    continue;
                }
                let tool_id = tool_call_id(tool_call);
                let tool_name = tool_call_name(tool_call);
                if skipped_tool_ids.contains(&tool_id) || is_openviking_recall_tool_name(&tool_name)
                {
                    continue;
                }
                if completed_tool_ids.contains(&tool_id) {
                    continue;
                }
                let tool_input = tool_calls_by_id
                    .get(&tool_id)
                    .map(|(_, input)| input.clone())
                    .unwrap_or_else(|| tool_call_input(tool_call));
                parts.push(json!({
                    "type": "tool",
                    "tool_id": tool_id,
                    "tool_name": tool_name,
                    "tool_input": tool_input,
                    "tool_status": TOOL_STATUS_PENDING,
                }));
            }
        }

        if !parts.is_empty() {
            payload_messages.push(payload_message(role, parts, assistant_peer_id));
        }
    }
    flush_tool_parts(&mut payload_messages, &mut pending_tool_parts);
    payload_messages
}

fn fallback_turn_batch(
    user_content: &str,
    assistant_content: &str,
    assistant_peer_id: &str,
) -> Vec<Value> {
    let mut messages = Vec::new();
    if !user_content.trim().is_empty() {
        messages.push(payload_message(
            "user",
            vec![json!({"type": "text", "text": user_content.chars().take(4000).collect::<String>()})],
            None,
        ));
    }
    if !messages.is_empty() {
        messages.push(payload_message(
            "assistant",
            vec![json!({"type": "text", "text": assistant_content.chars().take(4000).collect::<String>()})],
            Some(assistant_peer_id),
        ));
    }
    messages
}

fn post_openviking_batch(st: &VikingState, batch_messages: &[Value]) -> Result<(), String> {
    if batch_messages.is_empty() {
        return Ok(());
    }
    let url = format!(
        "{}/api/v1/sessions/{}/messages/batch",
        st.endpoint, st.session_id
    );
    let resp = st
        .client
        .post(&url)
        .headers(viking_headers(st))
        .json(&json!({"messages": batch_messages}))
        .send()
        .map_err(|e| format!("OpenViking structured sync failed: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("OpenViking structured sync HTTP {}", resp.status()))
    }
}

fn post_openviking_text_turn(
    st: &VikingState,
    user_content: &str,
    assistant_content: &str,
) -> Result<(), String> {
    let url = format!("{}/api/v1/sessions/{}/messages", st.endpoint, st.session_id);
    let user_status = st
        .client
        .post(&url)
        .headers(viking_headers(st))
        .json(&json!({"role": "user", "content": user_content.chars().take(4000).collect::<String>()}))
        .send()
        .map_err(|e| format!("OpenViking text user sync failed: {e}"))?
        .status();
    if !user_status.is_success() {
        return Err(format!("OpenViking text user sync HTTP {user_status}"));
    }

    let assistant_status = st
        .client
        .post(&url)
        .headers(viking_headers(st))
        .json(&json!({"role": "assistant", "content": assistant_content.chars().take(4000).collect::<String>()}))
        .send()
        .map_err(|e| format!("OpenViking text assistant sync failed: {e}"))?
        .status();
    if assistant_status.is_success() {
        Ok(())
    } else {
        Err(format!(
            "OpenViking text assistant sync HTTP {assistant_status}"
        ))
    }
}

