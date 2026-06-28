fn params_obj(params: &Option<Value>) -> Option<&serde_json::Map<String, Value>> {
    params.as_ref()?.as_object()
}

fn param_str<'a>(p: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    p.get(key)?.as_str()
}

fn param_str_any<'a>(p: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| param_str(p, key))
}

fn session_meta_from_params(
    p: &serde_json::Map<String, Value>,
) -> Result<SessionMetaUpdate, String> {
    let model = param_str_any(p, &["model", "modelId", "model_id"])
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned);
    let provider = param_str(p, "provider")
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(ToOwned::to_owned);
    let api_mode = param_str_any(p, &["apiMode", "api_mode"])
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(ToOwned::to_owned);
    let base_url = param_str_any(p, &["baseUrl", "base_url"])
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToOwned::to_owned);
    let profile = match param_str(p, "profile").map(str::trim) {
        Some("") | None => None,
        Some(profile) if valid_session_profile(profile) => Some(profile.to_string()),
        Some(profile) => {
            return Err(format!(
                "invalid profile '{}': use letters, numbers, '-', '_' or '.'",
                profile
            ))
        }
    };
    let home = param_str_any(
        p,
        &["home", "homeDir", "home_dir", "profileHome", "profile_home"],
    )
    .map(str::trim)
    .filter(|home| !home.is_empty())
    .map(ToOwned::to_owned);
    let title = param_str(p, "title")
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned);
    let mut config_options = HashMap::new();
    for (key, aliases) in [
        (
            "reasoning_effort",
            &["reasoningEffort", "reasoning_effort"][..],
        ),
        ("service_tier", &["serviceTier", "service_tier"][..]),
    ] {
        if let Some(value) = param_str_any(p, aliases)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            config_options.insert(key.to_string(), value.to_string());
        }
    }
    Ok(SessionMetaUpdate {
        model,
        provider,
        api_mode,
        base_url,
        profile,
        home,
        title,
        config_options,
        ..SessionMetaUpdate::default()
    })
}

fn valid_session_profile(profile: &str) -> bool {
    !profile.is_empty()
        && !profile.contains('/')
        && !profile.contains('\\')
        && profile
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn param_value_as_string(p: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    let value = p.get(key)?;
    if let Some(s) = value.as_str() {
        Some(s.to_string())
    } else {
        Some(value.to_string())
    }
}

fn param_value_as_string_any(p: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| param_value_as_string(p, key))
}

fn slash_command_parts(text: &str) -> Option<(String, &str)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let (cmd, args) = rest
        .split_once(char::is_whitespace)
        .map(|(cmd, args)| (cmd, args.trim()))
        .unwrap_or((rest, ""));
    let cmd = cmd.split('@').next().unwrap_or(cmd).trim();
    (!cmd.is_empty()).then(|| (cmd.to_ascii_lowercase().replace('-', "_"), args))
}

fn content_value_to_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(parts) = value.as_array() {
        let text = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        return (!text.trim().is_empty()).then_some(text);
    }
    None
}

fn latest_user_prompt_text(history: &[Value]) -> Option<String> {
    history.iter().rev().find_map(|message| {
        (message.get("role").and_then(Value::as_str) == Some("user"))
            .then(|| message.get("content").and_then(content_value_to_text))
            .flatten()
    })
}

fn flatten_history_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(text.trim());
                }
                let text = part
                    .get("text")
                    .or_else(|| {
                        (part.get("type").and_then(Value::as_str) == Some("text"))
                            .then(|| part.get("content"))
                            .flatten()
                    })
                    .and_then(Value::as_str)?;
                Some(text.trim())
            })
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn history_message_text(message: &Value) -> String {
    flatten_history_text(message.get("content"))
}

fn history_reasoning_text(message: &Value) -> String {
    ["reasoning_content", "reasoning"]
        .iter()
        .map(|key| flatten_history_text(message.get(*key)))
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

fn history_tool_call_arguments(tool_call: &Value) -> Option<Value> {
    let raw = tool_call
        .get("function")
        .and_then(|function| function.get("arguments"))
        .or_else(|| tool_call.get("arguments"))?;
    match raw {
        Value::String(text) => serde_json::from_str(text)
            .ok()
            .or_else(|| Some(json!(text))),
        Value::Null => None,
        other => Some(other.clone()),
    }
}

fn history_tool_call_name(tool_call: &Value) -> String {
    tool_call
        .get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool_call.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("tool")
        .to_string()
}

fn session_info_refresh_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn merge_usage(left: Option<Usage>, right: Option<Usage>) -> Option<Usage> {
    match (left, right) {
        (None, None) => None,
        (Some(usage), None) | (None, Some(usage)) => Some(usage),
        (Some(mut left), Some(right)) => {
            left.input_tokens += right.input_tokens;
            left.output_tokens += right.output_tokens;
            left.total_tokens += right.total_tokens;
            left.thought_tokens = match (left.thought_tokens, right.thought_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            left.cached_read_tokens = match (left.cached_read_tokens, right.cached_read_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            Some(left)
        }
    }
}

fn prompt_response_value(stop_reason: StopReason, usage: Option<Usage>) -> Value {
    serde_json::to_value(PromptResponse { stop_reason, usage }).unwrap_or_else(|_| json!({}))
}

fn session_id_response(session_id: &str) -> Value {
    json!({"sessionId": session_id})
}

fn format_token_count_plain(value: u64) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (idx, ch) in raw.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn session_title_from_history(history: &[Value]) -> Option<String> {
    history.iter().find_map(|message| {
        let role = message.get("role").and_then(Value::as_str)?;
        if role != "user" {
            return None;
        }
        let content = message.get("content")?;
        let text = if let Some(text) = content.as_str() {
            text.to_string()
        } else if let Some(parts) = content.as_array() {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::new()
        };
        let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
        (!title.is_empty()).then(|| {
            if title.chars().count() > 80 {
                format!("{}...", title.chars().take(77).collect::<String>())
            } else {
                title
            }
        })
    })
}

fn session_display_title(session: &SessionState) -> Option<String> {
    session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| session_title_from_history(&session.history))
}

fn session_info_value(session: &SessionState) -> Value {
    json!({
        "sessionId": session.session_id,
        "session_id": session.session_id,
        "cwd": session.cwd,
        "model": session.model,
        "provider": session.provider,
        "apiMode": session.api_mode,
        "baseUrl": session.base_url,
        "profile": session.profile,
        "home": session.home,
        "phase": session.phase,
        "historyLen": session.history.len(),
        "createdAt": session.created_at.to_string(),
        "updatedAt": session.updated_at.to_string(),
        "title": session_display_title(session),
    })
}

