fn value_to_display_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    return serde_json::to_string_pretty(&parsed)
                        .unwrap_or_else(|_| raw.to_string());
                }
            }
            raw.to_string()
        }
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn push_block(lines: &mut Vec<String>, header: &str, value: &serde_json::Value) {
    let rendered = value_to_display_text(value);
    if rendered.trim().is_empty() {
        return;
    }
    lines.push(format!("[{header}]"));
    for line in rendered.lines() {
        lines.push(line.to_string());
    }
}

fn sanitize_tool_line(raw: &str) -> String {
    let sanitized =
        sanitize_line_to_default_language_ascii(raw, false).unwrap_or_else(|| String::new());
    truncate_chars(&sanitized, max_tool_output_line_chars())
}

fn finalize_tool_message_lines(raw_lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut total_chars = 0usize;
    let mut omitted = 0usize;
    let max_lines = max_tool_output_lines();
    let max_total_chars = max_tool_output_total_chars();
    for line in raw_lines {
        let sanitized = sanitize_tool_line(&line);
        let line_chars = sanitized.chars().count();
        let next_total = total_chars.saturating_add(line_chars);
        if out.len() < max_lines && next_total <= max_total_chars {
            total_chars = next_total;
            out.push(sanitized);
        } else {
            omitted = omitted.saturating_add(1);
        }
    }
    if omitted > 0 {
        out.push(format!(
            "… tool output truncated ({} lines omitted)",
            omitted
        ));
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn format_tool_message_lines(content: &str) -> Vec<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return vec![String::new()];
    }

    let parsed = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return finalize_tool_message_lines(
                content
                    .lines()
                    .map(std::string::ToString::to_string)
                    .collect(),
            );
        }
    };

    if let Some(obj) = parsed.as_object() {
        let mut lines: Vec<String> = Vec::new();

        if let Some(w) = obj.get("_budget_warning").and_then(|v| v.as_str()) {
            lines.push(format!("⚠ {}", w.trim()));
        }

        for key in ["result", "error", "stdout", "stderr", "message"] {
            if let Some(value) = obj.get(key) {
                push_block(&mut lines, key, value);
            }
        }
        if let Some(remediation) = tool_policy_remediation_from_payload(obj) {
            lines.push("[remediation]".to_string());
            for row in remediation {
                lines.push(format!("- {}", row));
            }
        }

        let mut extras = serde_json::Map::new();
        for (k, v) in obj.iter() {
            if k == "_budget_warning"
                || k == "result"
                || k == "error"
                || k == "stdout"
                || k == "stderr"
                || k == "message"
            {
                continue;
            }
            extras.insert(k.clone(), v.clone());
        }
        if !extras.is_empty() {
            push_block(&mut lines, "meta", &serde_json::Value::Object(extras));
        }
        if !lines.is_empty() {
            return finalize_tool_message_lines(lines);
        }
    }

    finalize_tool_message_lines(
        serde_json::to_string_pretty(&parsed)
            .map(|s| s.lines().map(std::string::ToString::to_string).collect())
            .unwrap_or_else(|_| {
                content
                    .lines()
                    .map(std::string::ToString::to_string)
                    .collect()
            }),
    )
}

fn tool_policy_remediation_from_payload(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<Vec<String>> {
    let code = obj
        .get("policy")
        .and_then(|p| p.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let error_text = obj
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let blocked = error_text.contains("blocked by tool policy")
        || error_text.contains("denied by security policy")
        || !code.is_empty();
    if !blocked {
        return None;
    }

    let mut rows = Vec::new();
    match code.as_str() {
        "params_pattern_denied" => {
            rows.push(
                "Remove secret-like parameter names from tool args; pass secrets via local env/vault.".to_string(),
            );
            rows.push(
                "Retry with sanitized args that reference variable names, not credential material."
                    .to_string(),
            );
        }
        "params_too_large" => {
            rows.push(
                "Reduce payload size and pass only minimal fields required by the tool."
                    .to_string(),
            );
        }
        "tool_denylisted" | "tool_not_allowlisted" => {
            rows.push(
                "Switch to an approved tool surface (`/tools`) for this operation.".to_string(),
            );
        }
        "sandbox_profile_violation" => {
            rows.push(
                "Command matched sandbox denial pattern; use a safer equivalent command path."
                    .to_string(),
            );
            rows.push(
                "If necessary, change runtime sandbox policy explicitly before retrying."
                    .to_string(),
            );
        }
        _ => {
            rows.push(
                "Review policy decision details in `/ops status` and retry with safer parameters."
                    .to_string(),
            );
        }
    }
    Some(rows)
}

