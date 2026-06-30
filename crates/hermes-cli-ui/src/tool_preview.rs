use serde_json::{Map, Value};

fn truncate_chars(input: &str, max_len: usize) -> String {
    if max_len == 0 || input.chars().count() <= max_len {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_len.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn oneline(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shell_basename(head: &str) -> &str {
    head.rsplit_once('/').map(|(_, tail)| tail).unwrap_or(head)
}

fn is_shell_silent_head(head: &str) -> bool {
    matches!(
        head,
        "cd" | "pushd"
            | "popd"
            | "export"
            | "set"
            | "unset"
            | "source"
            | "."
            | "true"
            | "false"
            | ":"
    )
}

fn is_shell_pipe_tail_head(head: &str) -> bool {
    matches!(head, "head" | "tail" | "wc" | "sort" | "uniq")
}

fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some('_') | Some('A'..='Z') | Some('a'..='z'))
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn split_shell_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut prev = '\0';

    for ch in segment.chars() {
        if let Some(active_quote) = quote {
            buf.push(ch);
            if ch == active_quote && prev != '\\' {
                quote = None;
            }
            prev = ch;
            continue;
        }

        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            buf.push(ch);
        } else if ch.is_whitespace() {
            if !buf.is_empty() {
                words.push(std::mem::take(&mut buf));
            }
        } else {
            buf.push(ch);
        }
        prev = ch;
    }

    if !buf.is_empty() {
        words.push(buf);
    }
    words
}

fn strip_shell_pipe_tail(segment: &str) -> String {
    let words = split_shell_words(segment);
    let mut out = Vec::new();

    for (index, word) in words.iter().enumerate() {
        if word == "|"
            && words
                .get(index + 1)
                .map(|head| is_shell_pipe_tail_head(shell_basename(head)))
                .unwrap_or(false)
        {
            break;
        }
        out.push(word.as_str());
    }

    out.join(" ").trim().to_string()
}

fn split_shell_compound(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            buf.push(ch);
            if ch == active_quote && prev != '\\' {
                quote = None;
            }
            prev = ch;
            continue;
        }

        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            buf.push(ch);
            prev = ch;
            continue;
        }

        let compound_separator = match ch {
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                true
            }
            '|' if chars.peek() == Some(&'|') => {
                chars.next();
                true
            }
            ';' | '\n' => true,
            _ => false,
        };

        if compound_separator {
            let segment = strip_shell_pipe_tail(buf.trim());
            if !segment.is_empty() {
                segments.push(segment);
            }
            buf.clear();
        } else {
            buf.push(ch);
        }
        prev = ch;
    }

    let segment = strip_shell_pipe_tail(buf.trim());
    if !segment.is_empty() {
        segments.push(segment);
    }
    segments
}

fn shell_head_word(segment: &str) -> String {
    split_shell_words(segment)
        .into_iter()
        .find(|word| !is_env_assignment(word))
        .map(|word| shell_basename(&word).to_string())
        .unwrap_or_default()
}

fn is_redirect_operator(word: &str) -> bool {
    let stripped = word.trim_start_matches(|ch: char| ch.is_ascii_digit());
    matches!(stripped, ">" | ">>" | "<")
}

fn is_redirect_dup(word: &str) -> bool {
    let stripped = word.trim_start_matches(|ch: char| ch.is_ascii_digit());
    let Some((op, fd)) = stripped.split_once('&') else {
        return false;
    };
    matches!(op, ">" | "<") && fd.chars().all(|ch| ch.is_ascii_digit())
}

fn clean_shell_segment(segment: &str) -> String {
    let words = split_shell_words(segment);
    let mut out = Vec::new();
    let mut index = 0;

    while index < words.len() {
        let word = &words[index];
        if is_redirect_operator(word) {
            index += 2;
            continue;
        }
        if is_redirect_dup(word) {
            index += 1;
            continue;
        }
        out.push(word.as_str());
        index += 1;
    }

    out.join(" ").trim().to_string()
}

fn is_shell_boundary_echo(segment: &str) -> bool {
    let words = split_shell_words(segment);
    if words
        .first()
        .map(|word| shell_basename(word) == "echo")
        .unwrap_or(false)
    {
        let rest = words
            .iter()
            .skip(1)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        return rest.contains("--")
            || rest.contains("_exit=")
            || rest.contains("exit=")
            || rest.contains("$?")
            || rest.contains("${PIPESTATUS")
            || rest.contains("PIPESTATUS");
    }
    false
}

fn summarize_shell_command(command: &str) -> String {
    let original = oneline(command);
    if original.is_empty() {
        return String::new();
    }

    let segments = split_shell_compound(&original);
    if segments.len() <= 1 {
        let cleaned =
            clean_shell_segment(segments.first().map(String::as_str).unwrap_or(&original));
        return if cleaned.is_empty() {
            original
        } else {
            cleaned
        };
    }

    let core = segments
        .iter()
        .filter_map(|segment| {
            let cleaned = clean_shell_segment(segment);
            let head = shell_head_word(&cleaned);
            if cleaned.is_empty() || is_shell_silent_head(&head) || is_shell_boundary_echo(&cleaned)
            {
                None
            } else {
                Some(cleaned)
            }
        })
        .collect::<Vec<_>>();

    match core.as_slice() {
        [] => original,
        [single] => single.clone(),
        [first, rest @ ..] => format!(
            "{} + {} {}",
            first,
            rest.len(),
            if rest.len() == 1 {
                "command"
            } else {
                "commands"
            }
        ),
    }
}

fn path_basename(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(normalized.as_str())
        .to_string()
}

fn value_to_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

fn read_file_line_label(map: &Map<String, Value>) -> Option<String> {
    let offset = map.get("offset").and_then(value_to_i64)?;
    if offset <= 0 {
        return None;
    }
    let limit = map.get("limit").and_then(value_to_i64).unwrap_or(0);
    if limit <= 1 {
        Some(format!("L{offset}"))
    } else {
        Some(format!("L{}-{}", offset, offset + limit - 1))
    }
}

fn read_file_preview(map: &Map<String, Value>) -> Option<String> {
    let path = map
        .get("path")
        .or_else(|| map.get("file"))
        .or_else(|| map.get("filepath"))
        .and_then(value_to_scalar_string)?;
    let label = path_basename(path.trim());
    if label.is_empty() {
        return None;
    }
    Some(match read_file_line_label(map) {
        Some(line_label) => format!("{label} {line_label}"),
        None => label,
    })
}

pub fn tool_friendly_verb(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "web_search" => Some("Searching the web"),
        "web_extract" => Some("Reading"),
        "web_crawl" => Some("Crawling"),
        "browser_navigate" => Some("Browsing"),
        "browser_click" => Some("Clicking"),
        "browser_type" => Some("Typing"),
        "browser_scroll" => Some("Scrolling"),
        "browser_snapshot" => Some("Capturing"),
        "read_file" => Some("Reading"),
        "write_file" => Some("Writing"),
        "patch" => Some("Editing"),
        "search_files" => Some("Searching files"),
        "terminal" => Some("Running"),
        "execute_code" => Some("Running code"),
        "image_generate" => Some("Generating image"),
        "video_generate" => Some("Generating video"),
        "text_to_speech" => Some("Generating speech"),
        "vision_analyze" => Some("Looking at the image"),
        "video_analyze" => Some("Looking at the video"),
        "session_search" => Some("Searching past sessions"),
        "skill_view" => Some("Reading skill"),
        "skills_list" => Some("Listing skills"),
        "skill_manage" => Some("Updating skill"),
        "delegate_task" => Some("Delegating"),
        "schedule_cronjob" | "cronjob" => Some("Scheduling"),
        "list_cronjobs" => Some("Listing cron jobs"),
        "remove_cronjob" => Some("Removing cron job"),
        "clarify" => Some("Asking"),
        "memory" => Some("Updating memory"),
        "todo" => Some("Updating tasks"),
        "send_message" => Some("Sending message"),
        _ => None,
    }
}

pub fn tool_friendly_connector(tool_name: &str) -> &'static str {
    match tool_name {
        "web_search" | "search_files" | "spotify_search" => " for ",
        _ => " ",
    }
}

pub fn tool_friendly_drops_preview(tool_name: &str) -> bool {
    matches!(tool_name, "skills_list" | "session_search")
}

pub fn build_tool_label_from_preview(tool_name: &str, preview: Option<&str>) -> Option<String> {
    let verb = tool_friendly_verb(tool_name)?;
    if tool_friendly_drops_preview(tool_name) {
        return Some(verb.to_string());
    }
    let preview = preview.map(str::trim).filter(|value| !value.is_empty());
    Some(match preview {
        Some(preview) => format!("{verb}{}{preview}", tool_friendly_connector(tool_name)),
        None => verb.to_string(),
    })
}

pub fn build_tool_label_from_value(
    tool_name: &str,
    args: &Value,
    max_len: usize,
    friendly_labels: bool,
) -> Option<String> {
    let preview = build_tool_preview_from_value(tool_name, args, max_len);
    if !friendly_labels {
        return preview;
    }
    build_tool_label_from_preview(tool_name, preview.as_deref()).or(preview)
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
        "web_crawl" => "🕸️",
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
        "video_analyze" => "🎬",
        "video_generate" => "🎞️",
        "spotify_playback" => "🎵",
        "spotify_devices" => "🔈",
        "spotify_queue" => "📻",
        "spotify_search" => "🔎",
        "spotify_playlists" => "📚",
        "spotify_albums" => "💿",
        "spotify_library" => "❤️",
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

    if matches!(tool_name, "terminal" | "execute_code") {
        let key = if tool_name == "execute_code" {
            "code"
        } else {
            "command"
        };
        let command = map.get(key).and_then(value_to_scalar_string)?;
        let preview = summarize_shell_command(&command);
        return (!preview.trim().is_empty()).then(|| truncate_chars(&preview, max_len));
    }

    if tool_name == "read_file" {
        return read_file_preview(map).map(|preview| truncate_chars(&preview, max_len));
    }

    let primary_key = match tool_name {
        "terminal" => Some("command"),
        "execute_code" => Some("code"),
        "web_search" => Some("query"),
        "web_extract" => Some("urls"),
        "web_crawl" => Some("url"),
        "read_file" | "write_file" | "patch" => Some("path"),
        "search_files" => Some("pattern"),
        "browser_navigate" => Some("url"),
        "browser_click" => Some("ref"),
        "browser_type" => Some("text"),
        "image_generate" => Some("prompt"),
        "text_to_speech" => Some("text"),
        "vision_analyze" => Some("question"),
        "video_analyze" => Some("question"),
        "video_generate" => Some("prompt"),
        "spotify_search" => Some("query"),
        "spotify_playlists" => Some("name"),
        "spotify_queue" => Some("uri"),
        "spotify_albums" => Some("album_id"),
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

fn fenced_code_block(language: &str, body: &str) -> String {
    let body = body.trim_end();
    let fence = if body.contains("```") { "````" } else { "```" };
    format!("{fence}{language}\n{body}\n{fence}")
}

pub fn platform_supports_markdown_code_blocks(platform: &str) -> bool {
    matches!(
        platform
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .as_str(),
        "telegram"
            | "slack"
            | "matrix"
            | "whatsapp"
            | "feishu"
            | "weixin"
            | "discord"
            | "mattermost"
    )
}

pub fn build_gateway_tool_progress_message(
    platform: &str,
    tool_name: &str,
    args: &Value,
    mode: &str,
    max_len: usize,
) -> Option<String> {
    build_gateway_tool_progress_message_with_labels(platform, tool_name, args, mode, max_len, true)
}

pub fn build_gateway_tool_progress_message_with_labels(
    platform: &str,
    tool_name: &str,
    args: &Value,
    mode: &str,
    max_len: usize,
    friendly_labels: bool,
) -> Option<String> {
    let mode = mode.trim().to_ascii_lowercase();
    if matches!(mode.as_str(), "" | "off" | "none" | "false" | "0") {
        return None;
    }

    let supports_code_blocks = platform_supports_markdown_code_blocks(platform);
    if supports_code_blocks && tool_name == "terminal" {
        if let Some(command) = args
            .as_object()
            .and_then(|map| map.get("command"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|command| !command.is_empty())
        {
            return Some(fenced_code_block("bash", command));
        }
    }

    let emoji = tool_emoji(tool_name);
    if mode == "verbose" {
        let rendered_args = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
        if supports_code_blocks && rendered_args.trim() != "null" {
            return Some(format!(
                "{emoji} {tool_name}:\n{}",
                fenced_code_block("json", &rendered_args)
            ));
        }
        let cap = max_len.max(40);
        return Some(format!(
            "{emoji} {tool_name}: {}",
            truncate_chars(&rendered_args, cap)
        ));
    }

    if !friendly_labels {
        return match build_tool_preview_from_value(tool_name, args, max_len.max(40)) {
            Some(preview) if !preview.trim().is_empty() => {
                Some(format!("{emoji} {tool_name}: {preview}"))
            }
            _ => Some(format!("{emoji} {tool_name}")),
        };
    }

    match build_tool_label_from_value(tool_name, args, max_len.max(40), true) {
        Some(label) if !label.trim().is_empty() => Some(format!("{emoji} {label}")),
        _ => Some(format!("{emoji} {tool_name}")),
    }
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
    fn friendly_tool_labels_phrase_builtin_tools_and_preserve_custom_previews() {
        let web =
            build_tool_label_from_value("web_search", &json!({"query":"example search"}), 80, true)
                .unwrap();
        assert_eq!(web, "Searching the web for example search");

        let terminal = build_tool_label_from_value(
            "terminal",
            &json!({"command":"cd /repo && cargo test --workspace --quiet 2>&1 | tail -20"}),
            80,
            true,
        )
        .unwrap();
        assert_eq!(terminal, "Running cargo test --workspace --quiet");

        let skills =
            build_tool_label_from_value("skills_list", &json!({"category":"creative"}), 80, true)
                .unwrap();
        assert_eq!(skills, "Listing skills");

        let disabled = build_tool_label_from_value(
            "web_search",
            &json!({"query":"example search"}),
            80,
            false,
        )
        .unwrap();
        assert_eq!(disabled, "example search");

        let custom = build_tool_label_from_value(
            "custom_provider_search",
            &json!({"query":"semantic index"}),
            80,
            true,
        )
        .unwrap();
        assert_eq!(custom, "semantic index");
    }

    #[test]
    fn terminal_preview_compacts_shell_plumbing() {
        let preview = build_tool_preview_from_value(
            "terminal",
            &json!({
                "command": "cd /Users/brooklyn/www/bb-rainbows && pnpm run lint 2>&1 | tail -20; echo \"lint_exit=${PIPESTATUS[0]}\""
            }),
            80,
        )
        .unwrap();

        assert_eq!(preview, "pnpm run lint");
    }

    #[test]
    fn terminal_preview_compacts_multi_command_probe() {
        let preview = build_tool_preview_from_value(
            "terminal",
            &json!({
                "command": "which node pnpm corepack; node -v; echo \"---\"; corepack --version 2>&1; echo \"---pnpm via corepack---\"; pnpm --version 2>&1 | tail -5"
            }),
            80,
        )
        .unwrap();

        assert_eq!(preview, "which node pnpm corepack + 3 commands");
    }

    #[test]
    fn execute_code_preview_uses_shell_summary() {
        let preview = build_tool_preview_from_value(
            "execute_code",
            &json!({"code": "cd /tmp/demo && python -m pytest -q 2>&1 | tail -5; echo \"exit=$?\""}),
            80,
        )
        .unwrap();

        assert_eq!(preview, "python -m pytest -q");
    }

    #[test]
    fn read_file_preview_uses_basename_and_requested_line_range() {
        let preview = build_tool_preview_from_value(
            "read_file",
            &json!({"path":"./src/main.ts", "offset":25, "limit":10}),
            80,
        )
        .unwrap();

        assert_eq!(preview, "main.ts L25-34");
    }

    #[test]
    fn read_file_preview_accepts_string_line_numbers() {
        let preview = build_tool_preview_from_value(
            "read_file",
            &json!({"path":"C:\\repo\\package.json", "offset":"1", "limit":"5"}),
            80,
        )
        .unwrap();

        assert_eq!(preview, "package.json L1-5");
    }

    #[test]
    fn emoji_map_covers_process_and_todo() {
        assert_eq!(tool_emoji("process"), "⚙️");
        assert_eq!(tool_emoji("todo"), "📋");
        assert_eq!(tool_emoji("video_generate"), "🎞️");
        assert_eq!(tool_emoji("spotify_playback"), "🎵");
        assert_eq!(tool_emoji("unknown"), "⚙️");
    }

    #[test]
    fn gateway_tool_progress_uses_bash_blocks_for_terminal_on_markdown_platforms() {
        let msg = build_gateway_tool_progress_message(
            "telegram",
            "terminal",
            &json!({"command":"cargo test --workspace --quiet", "timeout": 120}),
            "all",
            10,
        )
        .unwrap();

        assert_eq!(msg, "```bash\ncargo test --workspace --quiet\n```");
    }

    #[test]
    fn gateway_tool_progress_keeps_plain_platforms_compact() {
        let msg = build_gateway_tool_progress_message(
            "sms",
            "terminal",
            &json!({"command":"cd /repo && cargo test --workspace --quiet 2>&1 | tail -20; echo \"exit=$?\""}),
            "all",
            24,
        )
        .unwrap();

        assert!(msg.starts_with("💻 Running "));
        assert!(!msg.contains("```bash"));
        assert!(msg.contains("cargo test --workspace"));
        assert!(!msg.contains("tail -20"));
    }

    #[test]
    fn gateway_tool_progress_can_disable_friendly_labels_for_legacy_debug() {
        let msg = build_gateway_tool_progress_message_with_labels(
            "sms",
            "web_search",
            &json!({"query":"example search"}),
            "all",
            80,
            false,
        )
        .unwrap();

        assert_eq!(msg, "🔍 web_search: example search");
    }

    #[test]
    fn gateway_tool_progress_falls_back_when_terminal_command_is_blank() {
        let msg = build_gateway_tool_progress_message(
            "telegram",
            "terminal",
            &json!({"command":"   "}),
            "verbose",
            80,
        )
        .unwrap();

        assert!(msg.starts_with("💻 terminal:\n```json"));
        assert!(msg.contains("\"command\""));
    }
}
