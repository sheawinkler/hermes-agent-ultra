use serde_json::Value;

fn truncate_chars(input: &str, max_len: usize) -> String {
    if input.chars().count() <= max_len {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_len.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn value_to_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(arr) => arr.first().and_then(value_to_scalar_string),
        Value::Null | Value::Object(_) => None,
    }
}

pub fn tool_emoji(tool_name: &str) -> &'static str {
    match tool_name {
        "terminal" => "💻",
        "process" => "⚙️",
        "web_search" => "🔍",
        "web_extract" => "📄",
        "read_file" => "📖",
        "write_file" => "✍️",
        "patch" => "🩹",
        "search_files" => "🔎",
        "browser_navigate" => "🌐",
        "browser_snapshot" => "📸",
        "browser_click" => "👆",
        "browser_type" => "⌨️",
        "browser_scroll" => "📜",
        "browser_back" => "◀️",
        "browser_press" => "⌨️",
        "browser_close" => "🚪",
        "browser_get_images" => "🖼️",
        "browser_vision" => "👁️",
        "vision_analyze" => "👁️",
        "mixture_of_agents" => "🧠",
        "todo" => "📋",
        "send_message" => "📨",
        "schedule_cronjob" | "list_cronjobs" | "remove_cronjob" => "⏰",
        _ => "⚙️",
    }
}

pub fn build_tool_preview_from_value(
    tool_name: &str,
    args: &Value,
    max_len: usize,
) -> Option<String> {
    let map = args.as_object()?;

    if tool_name == "process" {
        let action = map
            .get("action")
            .and_then(value_to_scalar_string)
            .unwrap_or_default();
        let session_id = map
            .get("session_id")
            .or_else(|| map.get("pid"))
            .and_then(value_to_scalar_string)
            .unwrap_or_default();
        let data = map
            .get("data")
            .or_else(|| map.get("input"))
            .and_then(value_to_scalar_string)
            .unwrap_or_default();
        let timeout = map.get("timeout").and_then(value_to_scalar_string);
        let mut parts = Vec::new();
        if !action.is_empty() {
            parts.push(action);
        }
        if !session_id.is_empty() {
            parts.push(truncate_chars(&session_id, 16));
        }
        if !data.is_empty() {
            parts.push(format!("\"{}\"", truncate_chars(&data, 20)));
        }
        if timeout.is_some() && map.get("action").and_then(Value::as_str) == Some("wait") {
            parts.push(format!("{}s", timeout.unwrap_or_default()));
        }
        if parts.is_empty() {
            return None;
        }
        return Some(parts.join(" "));
    }

    if tool_name == "todo" {
        if map.get("todos").is_none() {
            return Some("reading task list".to_string());
        }
        let count = map
            .get("todos")
            .and_then(Value::as_array)
            .map(|v| v.len())
            .unwrap_or(0);
        let merge = map.get("merge").and_then(Value::as_bool).unwrap_or(false);
        return Some(if merge {
            format!("updating {count} task(s)")
        } else {
            format!("planning {count} task(s)")
        });
    }

    if tool_name == "send_message" {
        let target = map
            .get("target")
            .and_then(value_to_scalar_string)
            .unwrap_or_else(|| "?".to_string());
        let message = map
            .get("message")
            .and_then(value_to_scalar_string)
            .unwrap_or_default();
        return Some(format!(
            "to {}: \"{}\"",
            target,
            truncate_chars(&message, 20)
        ));
    }

    if tool_name.starts_with("rl_") {
        let preview = match tool_name {
            "rl_list_environments" => Some("listing envs".to_string()),
            "rl_select_environment" => map.get("name").and_then(value_to_scalar_string),
            "rl_get_current_config" => Some("reading config".to_string()),
            "rl_edit_config" => Some(format!(
                "{}={}",
                map.get("field")
                    .and_then(value_to_scalar_string)
                    .unwrap_or_default(),
                map.get("value")
                    .and_then(value_to_scalar_string)
                    .unwrap_or_default()
            )),
            "rl_start_training" => Some("starting".to_string()),
            "rl_check_status" | "rl_get_results" | "rl_stop_training" => map
                .get("run_id")
                .and_then(value_to_scalar_string)
                .map(|v| truncate_chars(&v, 16)),
            "rl_list_runs" => Some("listing runs".to_string()),
            "rl_test_inference" => Some(format!(
                "{} steps",
                map.get("num_steps")
                    .and_then(value_to_scalar_string)
                    .unwrap_or_else(|| "3".to_string())
            )),
            _ => None,
        };
        return preview.filter(|s| !s.trim().is_empty());
    }

    let primary_key = match tool_name {
        "terminal" => Some("command"),
        "web_search" => Some("query"),
        "web_extract" => Some("urls"),
        "read_file" | "write_file" | "patch" => Some("path"),
        "search_files" => Some("pattern"),
        "browser_navigate" => Some("url"),
        "browser_click" => Some("ref"),
        "browser_type" => Some("text"),
        "image_generate" => Some("prompt"),
        "text_to_speech" => Some("text"),
        "vision_analyze" => Some("question"),
        "mixture_of_agents" => Some("user_prompt"),
        "skill_view" => Some("name"),
        "skills_list" => Some("category"),
        "schedule_cronjob" => Some("name"),
        _ => None,
    };

    let mut preview = primary_key
        .and_then(|k| map.get(k))
        .and_then(value_to_scalar_string)
        .or_else(|| {
            ["query", "text", "command", "path", "name", "prompt"]
                .iter()
                .find_map(|k| map.get(*k).and_then(value_to_scalar_string))
        })?;
    preview = preview.trim().to_string();
    if preview.is_empty() {
        return None;
    }
    Some(truncate_chars(&preview, max_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn process_preview_includes_action_session_data_timeout() {
        let preview = build_tool_preview_from_value(
            "process",
            &json!({
                "action": "wait",
                "session_id": "proc_1234567890abcdef",
                "data": "line of input that is long",
                "timeout": 30
            }),
            40,
        )
        .unwrap();
        assert!(preview.starts_with("wait"));
        assert!(preview.contains("proc_"));
        assert!(preview.contains("30s"));
    }

    #[test]
    fn process_preview_uses_pid_alias() {
        let preview =
            build_tool_preview_from_value("process", &json!({"action":"kill","pid":12345}), 40)
                .unwrap();
        assert!(preview.contains("kill"));
        assert!(preview.contains("12345"));
    }

    #[test]
    fn todo_and_message_preview_are_descriptive() {
        let todo_preview = build_tool_preview_from_value(
            "todo",
            &json!({"todos":[{"text":"a"},{"text":"b"}]}),
            40,
        )
        .unwrap();
        assert_eq!(todo_preview, "planning 2 task(s)");

        let message_preview = build_tool_preview_from_value(
            "send_message",
            &json!({"target":"telegram:abc","message":"hello from test message"}),
            40,
        )
        .unwrap();
        assert!(message_preview.starts_with("to telegram:abc:"));
    }

    #[test]
    fn rl_preview_and_fallback_preview_work() {
        let rl_preview = build_tool_preview_from_value(
            "rl_check_status",
            &json!({"run_id":"run_12345678901234567890"}),
            40,
        )
        .unwrap();
        assert!(rl_preview.starts_with("run_"));
        assert!(rl_preview.len() <= 16);

        let fallback =
            build_tool_preview_from_value("web_search", &json!({"query":"example search"}), 40)
                .unwrap();
        assert_eq!(fallback, "example search");
    }

    #[test]
    fn emoji_map_covers_process_and_todo() {
        assert_eq!(tool_emoji("process"), "⚙️");
        assert_eq!(tool_emoji("todo"), "📋");
        assert_eq!(tool_emoji("unknown"), "⚙️");
    }
}
