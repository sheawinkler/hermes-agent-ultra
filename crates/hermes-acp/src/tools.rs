//! ACP tool metadata helpers.
//!
//! These helpers keep ACP tool events compact but informative for clients.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

const TOOL_KIND_MAP: &[(&str, &str)] = &[
    ("read_file", "read"),
    ("search_files", "search"),
    ("terminal", "execute"),
    ("bash", "execute"),
    ("process", "execute"),
    ("execute_code", "execute"),
    ("patch", "edit"),
    ("write_file", "edit"),
    ("web_search", "fetch"),
    ("web_extract", "fetch"),
    ("browser_navigate", "fetch"),
    ("browser_click", "fetch"),
    ("skill_view", "read"),
    ("skill_manage", "edit"),
    ("todo", "other"),
    ("memory", "other"),
    ("session_search", "read"),
    ("delegate_task", "other"),
];

static TOOL_CALL_IDS: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStartMetadata {
    pub kind: &'static str,
    pub title: String,
}

pub fn make_tool_call_id() -> String {
    let id = TOOL_CALL_IDS.fetch_add(1, Ordering::Relaxed);
    format!("tc-{id}")
}

pub fn tool_kind(tool_name: &str) -> &'static str {
    TOOL_KIND_MAP
        .iter()
        .find_map(|(name, kind)| (*name == tool_name).then_some(*kind))
        .unwrap_or("other")
}

pub fn tool_start_metadata(tool_name: &str, arguments: Option<&Value>) -> ToolStartMetadata {
    ToolStartMetadata {
        kind: tool_kind(tool_name),
        title: tool_title(tool_name, arguments),
    }
}

pub fn tool_title(tool_name: &str, arguments: Option<&Value>) -> String {
    let value = arguments.unwrap_or(&Value::Null);
    match tool_name {
        "terminal" | "bash" => value_string(value, "command")
            .map(|cmd| truncate_chars(&cmd, 110))
            .unwrap_or_else(|| tool_name.to_string()),
        "read_file" => value_string(value, "path")
            .map(|path| format!("read: {path}"))
            .unwrap_or_else(|| "read_file".to_string()),
        "search_files" => value_string(value, "pattern")
            .map(|pattern| format!("search: {pattern}"))
            .unwrap_or_else(|| "search_files".to_string()),
        "patch" | "write_file" => value_string(value, "path")
            .map(|path| format!("{tool_name}: {path}"))
            .unwrap_or_else(|| tool_name.to_string()),
        "web_search" => value_string(value, "query")
            .map(|query| format!("search: {query}"))
            .unwrap_or_else(|| "web_search".to_string()),
        "web_extract" => value_urls(value, "urls")
            .first()
            .map(|url| format!("extract: {url}"))
            .unwrap_or_else(|| "web_extract".to_string()),
        "browser_navigate" => value_string(value, "url")
            .map(|url| format!("navigate: {url}"))
            .unwrap_or_else(|| "browser_navigate".to_string()),
        "skill_view" => {
            let name = value_string(value, "name").unwrap_or_else(|| "unknown".to_string());
            match value_string(value, "file_path") {
                Some(file_path) if !file_path.trim().is_empty() => {
                    format!("skill view ({name}/{file_path})")
                }
                _ => format!("skill view ({name})"),
            }
        }
        "skill_manage" => {
            let action = value_string(value, "action").unwrap_or_else(|| "manage".to_string());
            let name = value_string(value, "name").unwrap_or_else(|| "unknown".to_string());
            match value_string(value, "file_path") {
                Some(file_path) if !file_path.trim().is_empty() => {
                    format!("skill {action}: {name}/{file_path}")
                }
                _ => format!("skill {action}: {name}"),
            }
        }
        "execute_code" => {
            let language = value_string(value, "language").unwrap_or_else(|| "code".to_string());
            value_string(value, "code")
                .and_then(|code| first_non_empty_line(&code))
                .map(|line| format!("{language}: {}", truncate_chars(&line, 90)))
                .unwrap_or_else(|| "execute_code".to_string())
        }
        "todo" => todo_title(value),
        other => other.to_string(),
    }
}

pub fn tool_completion_status(tool_name: &str, result: Option<&str>) -> &'static str {
    if result
        .map(|output| tool_output_failed(tool_name, output))
        .unwrap_or(false)
    {
        "failed"
    } else {
        "completed"
    }
}

pub fn format_tool_result(tool_name: &str, result: Option<&str>) -> Option<String> {
    let formatted = match tool_name {
        "todo" => format_todo_result(result),
        "read_file" => format_read_file_result(result),
        "search_files" => format_search_files_result(result),
        "execute_code" => format_execute_code_result(result),
        "skill_view" => format_skill_view_result(result),
        "skill_manage" | "write_file" | "patch" => format_edit_result(tool_name, result),
        "browser_navigate" | "browser_snapshot" | "browser_vision" | "browser_get_images" => {
            format_browser_result(tool_name, result)
        }
        "memory" | "process" | "delegate_task" | "session_search" | "web_search"
        | "web_extract" | "vision_analyze" | "image_generate" | "cronjob" => {
            format_generic_structured_result(tool_name, result, true)
        }
        _ => format_generic_structured_result(tool_name, result, false),
    };
    formatted.or_else(|| {
        result
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| truncate_text(text, 5000))
    })
}

pub fn tool_output_failed(tool_name: &str, output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.starts_with("Error executing tool '") {
        return true;
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return false;
    };
    let Some(obj) = value.as_object() else {
        return false;
    };

    if obj.get("success").and_then(Value::as_bool) == Some(false)
        || obj.get("ok").and_then(Value::as_bool) == Some(false)
    {
        return true;
    }
    if obj
        .get("exit_code")
        .or_else(|| obj.get("returncode"))
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
    {
        return true;
    }
    obj.contains_key("error")
        && matches!(
            tool_name,
            "read_file" | "write_file" | "patch" | "skill_manage" | "execute_code" | "terminal"
        )
}

fn json_loads_maybe(result: Option<&str>) -> Option<Value> {
    let text = result?.trim_start();
    if text.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(text).ok().or_else(|| {
        serde_json::Deserializer::from_str(text)
            .into_iter::<Value>()
            .next()
            .and_then(Result::ok)
    })
}

fn truncate_text(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let keep = limit.saturating_sub(80);
    let head: String = text.chars().take(keep).collect();
    format!(
        "{head}\n... ({} chars total, truncated)",
        text.chars().count()
    )
}

fn fenced_text(text: &str) -> String {
    let mut longest_run = 0usize;
    let mut current_run = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    let fence = "`".repeat(3.max(longest_run + 1));
    format!("{fence}\n{text}\n{fence}")
}

fn value_summary(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Array(items) => Some(format!(
            "{} item{}",
            items.len(),
            if items.len() == 1 { "" } else { "s" }
        )),
        Value::Object(map) => Some(format!(
            "{} field{}",
            map.len(),
            if map.len() == 1 { "" } else { "s" }
        )),
    }
}

fn format_todo_result(result: Option<&str>) -> Option<String> {
    let data = json_loads_maybe(result)?;
    let todos = data.get("todos")?.as_array()?;
    let mut lines = vec!["**Todo list**".to_string(), String::new()];
    for item in todos.iter().filter_map(Value::as_object) {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        let marker = match status {
            "completed" => "[x]",
            "in_progress" => "[~]",
            "cancelled" => "[-]",
            _ => "[ ]",
        };
        let content = item
            .get("content")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !content.is_empty() {
            lines.push(format!("- {marker} {content}"));
        }
    }
    if let Some(summary) = data.get("summary").and_then(Value::as_object) {
        let count = |key: &str| summary.get(key).and_then(Value::as_u64).unwrap_or(0);
        let cancelled = count("cancelled");
        let mut progress = format!(
            "**Progress:** {} completed, {} in progress, {} pending",
            count("completed"),
            count("in_progress"),
            count("pending")
        );
        if cancelled > 0 {
            progress.push_str(&format!(", {cancelled} cancelled"));
        }
        lines.extend([String::new(), progress]);
    }
    Some(lines.join("\n"))
}

fn format_read_file_result(result: Option<&str>) -> Option<String> {
    let data = json_loads_maybe(result)?;
    let obj = data.as_object()?;
    if obj.get("error").is_some() && !obj.contains_key("content") {
        return Some(format!(
            "Read failed: {}",
            obj.get("error").and_then(value_summary).unwrap_or_default()
        ));
    }
    let content = obj.get("content")?.as_str()?;
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .unwrap_or("file");
    let mut header = format!("Read {path}");
    if let Some(total_lines) = obj.get("total_lines").and_then(value_summary) {
        header.push_str(&format!(" - {total_lines} total lines"));
    }
    Some(truncate_text(
        &format!("{header}\n\n{}", fenced_text(content)),
        7000,
    ))
}

fn format_search_files_result(result: Option<&str>) -> Option<String> {
    let data = json_loads_maybe(result)?;
    let obj = data.as_object()?;
    if let Some(files) = obj.get("files").and_then(Value::as_array) {
        let total = obj
            .get("total_count")
            .and_then(Value::as_u64)
            .unwrap_or(files.len() as u64);
        let shown = files.len().min(20);
        let mut lines = vec![
            "File search results".to_string(),
            format!(
                "Found {total} file{}; showing {shown}.",
                if total == 1 { "" } else { "s" }
            ),
            String::new(),
        ];
        lines.extend(
            files
                .iter()
                .take(shown)
                .filter_map(Value::as_str)
                .map(|path| format!("- {path}")),
        );
        if obj
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || files.len() > shown
        {
            lines.extend([
                String::new(),
                "Results truncated. Narrow the search or use offset to page.".to_string(),
            ]);
        }
        return Some(truncate_text(&lines.join("\n"), 7000));
    }

    let matches = obj.get("matches")?.as_array()?;
    let total = obj
        .get("total_count")
        .and_then(Value::as_u64)
        .unwrap_or(matches.len() as u64);
    let shown = matches.len().min(12);
    let mut lines = vec![
        "Search results".to_string(),
        format!(
            "Found {total} match{}; showing {shown}.",
            if total == 1 { "" } else { "es" }
        ),
        String::new(),
    ];
    for item in matches.iter().take(shown) {
        if let Some(obj) = item.as_object() {
            let path = obj
                .get("path")
                .or_else(|| obj.get("file"))
                .or_else(|| obj.get("filename"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let line = obj
                .get("line")
                .or_else(|| obj.get("line_number"))
                .and_then(value_summary);
            let loc = line
                .map(|line| format!("{path}:{line}"))
                .unwrap_or_else(|| path.to_string());
            lines.push(format!("- {loc}"));
            if let Some(content) = obj
                .get("content")
                .or_else(|| obj.get("text"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                lines.push(format!(
                    "  {}",
                    truncate_text(
                        &content.split_whitespace().collect::<Vec<_>>().join(" "),
                        300
                    )
                ));
            }
        } else if let Some(summary) = value_summary(item) {
            lines.push(format!("- {summary}"));
        }
    }
    if obj
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || matches.len() > shown
    {
        lines.extend([
            String::new(),
            "Results truncated. Narrow the search, add file_glob, or use offset to page."
                .to_string(),
        ]);
    }
    Some(truncate_text(&lines.join("\n"), 7000))
}

fn format_execute_code_result(result: Option<&str>) -> Option<String> {
    let Some(data) = json_loads_maybe(result) else {
        return result
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| truncate_text(text, 5000));
    };
    let obj = data.as_object()?;
    let mut lines = vec![obj
        .get("exit_code")
        .or_else(|| obj.get("returncode"))
        .and_then(value_summary)
        .map(|code| format!("Exit code: {code}"))
        .unwrap_or_else(|| "Execution complete".to_string())];
    if let Some(output) = obj
        .get("output")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.extend([String::new(), "Output:".to_string(), output.to_string()]);
    }
    if let Some(error) = obj
        .get("error")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.extend([String::new(), "Error:".to_string(), error.to_string()]);
    }
    Some(truncate_text(&lines.join("\n"), 7000))
}

fn extract_markdown_headings(content: &str, limit: usize) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let stripped = line.trim();
            stripped
                .starts_with('#')
                .then(|| stripped.trim_start_matches('#').trim().to_string())
                .filter(|heading| !heading.is_empty())
        })
        .take(limit)
        .collect()
}

fn format_skill_view_result(result: Option<&str>) -> Option<String> {
    let data = json_loads_maybe(result)?;
    let obj = data.as_object()?;
    if obj.get("success").and_then(Value::as_bool) == Some(false) {
        return Some(format!(
            "Skill view failed: {}",
            obj.get("error")
                .and_then(value_summary)
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("skill");
    let file_path = obj
        .get("file")
        .or_else(|| obj.get("path"))
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .unwrap_or("SKILL.md");
    let mut lines = vec![
        "**Skill loaded**".to_string(),
        String::new(),
        format!("- **Name:** `{name}`"),
        format!("- **File:** `{file_path}`"),
    ];
    if let Some(description) = obj
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.push(format!("- **Description:** {description}"));
    }
    if let Some(content) = obj.get("content").and_then(Value::as_str) {
        lines.push(format!(
            "- **Content:** {} chars loaded into agent context",
            content.chars().count()
        ));
        let headings = extract_markdown_headings(content, 8);
        if !headings.is_empty() {
            lines.extend([String::new(), "**Sections**".to_string()]);
            lines.extend(headings.into_iter().map(|heading| format!("- {heading}")));
        }
    }
    lines.extend([
        String::new(),
        "_Full skill content is available to the agent but hidden here to keep ACP readable._"
            .to_string(),
    ]);
    Some(lines.join("\n"))
}

fn format_edit_result(tool_name: &str, result: Option<&str>) -> Option<String> {
    let Some(data) = json_loads_maybe(result) else {
        return result
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| truncate_text(text, 3000));
    };
    let obj = data.as_object()?;
    let path = obj
        .get("path")
        .or_else(|| obj.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or("file");
    if obj.get("success").and_then(Value::as_bool) == Some(false) || obj.contains_key("error") {
        return Some(format!(
            "{tool_name} failed for {path}: {}",
            obj.get("error")
                .and_then(value_summary)
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    let mut lines = vec![format!("{tool_name} completed for `{path}`")];
    if let Some(message) = obj
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.push(message.to_string());
    }
    if let Some(replacements) = obj
        .get("replacements")
        .or_else(|| obj.get("replacement_count"))
        .and_then(value_summary)
    {
        lines.push(format!("Replacements: {replacements}"));
    }
    Some(lines.join("\n"))
}

fn format_browser_result(tool_name: &str, result: Option<&str>) -> Option<String> {
    let Some(data) = json_loads_maybe(result) else {
        return result
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| truncate_text(text, 5000));
    };
    let obj = data.as_object()?;
    if obj.get("success").and_then(Value::as_bool) == Some(false) || obj.contains_key("error") {
        return Some(format!(
            "{tool_name} failed: {}",
            obj.get("error")
                .and_then(value_summary)
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    if tool_name == "browser_get_images" {
        if let Some(images) = obj
            .get("images")
            .or_else(|| obj.get("data"))
            .and_then(Value::as_array)
        {
            let mut lines = vec![format!("Images found: {}", images.len())];
            for image in images.iter().take(12).filter_map(Value::as_object) {
                let label = image
                    .get("alt")
                    .and_then(Value::as_str)
                    .filter(|alt| !alt.trim().is_empty())
                    .unwrap_or("image");
                let url = image
                    .get("url")
                    .or_else(|| image.get("src"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                lines.push(if url.is_empty() {
                    format!("- {label}")
                } else {
                    format!("- {label} - {url}")
                });
            }
            return Some(truncate_text(&lines.join("\n"), 5000));
        }
    }
    let title = obj
        .get("title")
        .or_else(|| obj.get("url"))
        .or_else(|| obj.get("status"))
        .and_then(value_summary)
        .unwrap_or_else(|| tool_name.to_string());
    let mut lines = vec![title.clone()];
    if let Some(url) = obj
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| *url != title)
    {
        lines.push(url.to_string());
    }
    if let Some(text) = obj
        .get("text")
        .or_else(|| obj.get("content"))
        .or_else(|| obj.get("snapshot"))
        .or_else(|| obj.get("analysis"))
        .or_else(|| obj.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.extend([String::new(), truncate_text(text, 5000)]);
    }
    Some(truncate_text(&lines.join("\n"), 7000))
}

fn format_structured_value(
    key: &str,
    value: &Value,
    indent: usize,
    max_depth: usize,
    max_items: usize,
) -> Vec<String> {
    if matches!(value, Value::Null) {
        return Vec::new();
    }
    if value.as_str().is_some_and(|text| text.trim().is_empty()) {
        return Vec::new();
    }

    let prefix = "  ".repeat(indent);
    let label = (!key.is_empty()).then(|| format!("**{key}:**"));
    if max_depth == 0 {
        let preview = value_summary(value).unwrap_or_else(|| value.to_string());
        return vec![if let Some(label) = label {
            format!("{prefix}- {label} {}", truncate_text(&preview, 240))
        } else {
            format!("{prefix}- {}", truncate_text(&preview, 240))
        }];
    }

    match value {
        Value::Object(map) => {
            let mut lines = vec![if let Some(label) = label {
                format!("{prefix}- {label}")
            } else {
                format!("{prefix}- {} fields", map.len())
            }];
            let mut shown = 0usize;
            for (child_key, child_value) in map {
                if matches!(child_value, Value::Null) {
                    continue;
                }
                lines.extend(format_structured_value(
                    child_key,
                    child_value,
                    indent + 1,
                    max_depth.saturating_sub(1),
                    max_items,
                ));
                shown += 1;
                if shown >= max_items {
                    let remaining = map.len().saturating_sub(shown);
                    if remaining > 0 {
                        lines.push(format!(
                            "{}- ... {remaining} more fields",
                            "  ".repeat(indent + 1)
                        ));
                    }
                    break;
                }
            }
            lines
        }
        Value::Array(items) => {
            let mut lines = vec![if let Some(label) = label {
                format!(
                    "{prefix}- {label} {} item{}",
                    items.len(),
                    if items.len() == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "{prefix}- {} item{}",
                    items.len(),
                    if items.len() == 1 { "" } else { "s" }
                )
            }];
            for (idx, item) in items.iter().take(max_items).enumerate() {
                if let Some(obj) = item.as_object() {
                    let headline = ["content", "message", "title", "name", "id"]
                        .iter()
                        .filter_map(|key| obj.get(*key).and_then(value_summary))
                        .find(|text| !text.trim().is_empty());
                    if let Some(headline) = headline {
                        lines.push(format!(
                            "{}{}. {}",
                            "  ".repeat(indent + 1),
                            idx + 1,
                            truncate_text(&headline, 220)
                        ));
                        for child_key in ["id", "status", "type", "scope", "score", "path", "url"] {
                            if let Some(child_value) = obj.get(child_key).and_then(value_summary) {
                                lines.push(format!(
                                    "{}- **{child_key}:** {}",
                                    "  ".repeat(indent + 2),
                                    truncate_text(&child_value, 180)
                                ));
                            }
                        }
                    } else {
                        lines.push(format!("{}{}.", "  ".repeat(indent + 1), idx + 1));
                        for (child_key, child_value) in obj.iter().take(max_items) {
                            lines.extend(format_structured_value(
                                child_key,
                                child_value,
                                indent + 2,
                                max_depth.saturating_sub(1),
                                max_items,
                            ));
                        }
                    }
                } else {
                    let preview = value_summary(item).unwrap_or_else(|| item.to_string());
                    lines.push(format!(
                        "{}{}. {}",
                        "  ".repeat(indent + 1),
                        idx + 1,
                        truncate_text(&preview, 240)
                    ));
                }
            }
            if items.len() > max_items {
                lines.push(format!(
                    "{}... {} more items",
                    "  ".repeat(indent + 1),
                    items.len() - max_items
                ));
            }
            lines
        }
        _ => {
            let preview = value_summary(value).unwrap_or_else(|| value.to_string());
            vec![if let Some(label) = label {
                format!("{prefix}- {label} {}", truncate_text(&preview, 500))
            } else {
                format!("{prefix}- {}", truncate_text(&preview, 500))
            }]
        }
    }
}

fn format_generic_structured_result(
    tool_name: &str,
    result: Option<&str>,
    fallback_to_text: bool,
) -> Option<String> {
    let data = json_loads_maybe(result)?;
    if let Some(items) = data.as_array() {
        let mut lines = vec![format!(
            "{tool_name}: {} item{}",
            items.len(),
            if items.len() == 1 { "" } else { "s" }
        )];
        for item in items.iter().take(12) {
            if matches!(item, Value::Object(_) | Value::Array(_)) {
                lines.extend(format_structured_value("", item, 0, 2, 6));
            } else if let Some(summary) = value_summary(item) {
                lines.push(format!("- {}", truncate_text(&summary, 240)));
            }
        }
        if items.len() > 12 {
            lines.push(format!("... {} more items", items.len() - 12));
        }
        return Some(truncate_text(&lines.join("\n"), 5000));
    }

    let Some(obj) = data.as_object() else {
        return result
            .filter(|_| fallback_to_text)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| truncate_text(text, 5000));
    };

    if obj.get("success").and_then(Value::as_bool) == Some(false) || obj.contains_key("error") {
        return Some(format!(
            "{tool_name} failed: {}",
            obj.get("error")
                .and_then(value_summary)
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }

    let mut lines = vec![
        if obj.get("success").and_then(Value::as_bool) == Some(true) {
            format!("{tool_name} completed")
        } else {
            format!("{tool_name} result")
        },
    ];
    let priority_keys = [
        "message",
        "status",
        "id",
        "task_id",
        "issue_id",
        "title",
        "name",
        "entity_id",
        "state",
        "service",
        "url",
        "path",
        "file_path",
        "count",
        "total",
        "next_run",
    ];
    let mut seen = std::collections::HashSet::new();
    for key in priority_keys {
        if let Some(value) = obj.get(key).and_then(value_summary) {
            seen.insert(key);
            lines.push(format!("- **{key}:** {}", truncate_text(&value, 500)));
        }
    }
    for (key, value) in obj {
        if seen.contains(key.as_str()) || matches!(key.as_str(), "success" | "raw" | "entries") {
            continue;
        }
        if let Some(text) = value.as_str() {
            if text.trim().is_empty() {
                continue;
            }
        }
        lines.extend(format_structured_value(key, value, 0, 3, 8));
        if lines.len() >= 40 {
            lines.push("- ... more fields truncated".to_string());
            break;
        }
    }
    if let Some(content) = obj
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.extend([String::new(), truncate_text(content, 1500)]);
    }
    Some(truncate_text(&lines.join("\n"), 7000))
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    let raw = value.get(key)?;
    if let Some(text) = raw.as_str() {
        Some(text.to_string())
    } else {
        Some(raw.to_string())
    }
    .map(|text| text.trim().to_string())
    .filter(|text| !text.is_empty())
}

fn value_urls(value: &Value, key: &str) -> Vec<String> {
    let Some(raw) = value.get(key) else {
        return Vec::new();
    };
    if let Some(url) = raw.as_str() {
        return vec![url.to_string()];
    }
    raw.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

fn todo_title(value: &Value) -> String {
    let count = value
        .get("todos")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    match count {
        1 => "todo (1 item)".to_string(),
        n => format!("todo ({n} items)"),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let head: String = text.chars().take(keep).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_kind_covers_common_hermes_tools() {
        for (tool, expected) in [
            ("read_file", "read"),
            ("search_files", "search"),
            ("terminal", "execute"),
            ("patch", "edit"),
            ("write_file", "edit"),
            ("process", "execute"),
            ("web_search", "fetch"),
            ("execute_code", "execute"),
            ("todo", "other"),
            ("skill_view", "read"),
            ("browser_navigate", "fetch"),
            ("unknown_tool", "other"),
        ] {
            assert_eq!(tool_kind(tool), expected);
        }
    }

    #[test]
    fn make_tool_call_id_uses_stable_prefix_and_unique_values() {
        let first = make_tool_call_id();
        let second = make_tool_call_id();
        assert!(first.starts_with("tc-"));
        assert!(second.starts_with("tc-"));
        assert_ne!(first, second);
    }

    #[test]
    fn tool_title_uses_human_readable_arguments() {
        assert_eq!(
            tool_title("terminal", Some(&json!({"command": "ls -la /tmp"}))),
            "ls -la /tmp"
        );
        assert_eq!(
            tool_title("read_file", Some(&json!({"path": "/etc/hosts"}))),
            "read: /etc/hosts"
        );
        assert_eq!(
            tool_title("search_files", Some(&json!({"pattern": "TODO"}))),
            "search: TODO"
        );
        assert_eq!(
            tool_title("web_search", Some(&json!({"query": "rust acp"}))),
            "search: rust acp"
        );
        assert_eq!(
            tool_title("browser_navigate", Some(&json!({"url": "https://x.com"}))),
            "navigate: https://x.com"
        );
        assert_eq!(
            tool_title(
                "skill_view",
                Some(&json!({"name": "github-pitfalls", "file_path": "references/api.md"}))
            ),
            "skill view (github-pitfalls/references/api.md)"
        );
        assert_eq!(
            tool_title(
                "execute_code",
                Some(&json!({"language": "rust", "code": "\nprintln!(\"hello\");"}))
            ),
            "rust: println!(\"hello\");"
        );
        assert_eq!(
            tool_title(
                "skill_manage",
                Some(&json!({"action": "patch", "name": "ops", "file_path": "ref.md"}))
            ),
            "skill patch: ops/ref.md"
        );
        assert_eq!(
            tool_title(
                "todo",
                Some(&json!({"todos": [{"id": "one", "content": "Fix ACP"}]}))
            ),
            "todo (1 item)"
        );
    }

    #[test]
    fn terminal_titles_are_truncated() {
        let title = tool_title("terminal", Some(&json!({"command": "x".repeat(200)})));
        assert!(title.len() < 120);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn format_tool_result_renders_todo_summary_without_raw_json() {
        let result = format_tool_result(
            "todo",
            Some(
                r#"{"todos":[{"id":"a","content":"Inspect ACP","status":"completed"},{"id":"b","content":"Patch renderers","status":"in_progress"}],"summary":{"pending":0,"in_progress":1,"completed":1,"cancelled":0}}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("**Todo list**"));
        assert!(result.contains("- [x] Inspect ACP"));
        assert!(result.contains("- [~] Patch renderers"));
        assert!(result.contains("**Progress:** 1 completed, 1 in progress, 0 pending"));
        assert!(!result.contains(r#""todos""#));
    }

    #[test]
    fn format_tool_result_fences_read_file_content() {
        let result = format_tool_result(
            "read_file",
            Some(r#"{"path":"README.md","content":"1|hello\n2|world","total_lines":2}"#),
        )
        .expect("formatted");
        assert!(result.contains("Read README.md - 2 total lines"));
        assert!(result.contains("```\n1|hello\n2|world\n```"));
        assert!(!result.contains(r#""content""#));
    }

    #[test]
    fn format_tool_result_decodes_json_prefix_before_hint() {
        let result = format_tool_result(
            "search_files",
            Some(
                r#"{"total_count":2,"matches":[{"path":"README.md","line":3,"content":"TODO: fix this"},{"path":"src/app.rs","line":9,"content":"needle"}],"truncated":true}

[Hint: Results truncated. Use offset=12 to see more.]"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Search results"));
        assert!(result.contains("Found 2 matches"));
        assert!(result.contains("README.md:3"));
        assert!(result.contains("TODO: fix this"));
        assert!(result.contains("Results truncated"));
        assert!(!result.contains("[Hint:"));
    }

    #[test]
    fn format_tool_result_renders_generic_nested_json_compactly() {
        let result = format_tool_result(
            "custom_tool",
            Some(
                r#"{"success":true,"message":"ok","items":[{"id":"one","status":"done","details":{"score":0.98}},{"name":"two","url":"https://example.com"}],"content":"hidden body"}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("custom_tool completed"));
        assert!(result.contains("- **message:** ok"));
        assert!(result.contains("- **items:** 2 items"));
        assert!(result.contains("1. one"));
        assert!(result.contains("- **status:** done"));
        assert!(result.contains("hidden body"));
        assert!(!result.contains(r#""success""#));
    }

    #[test]
    fn completion_status_detects_structured_failures() {
        assert_eq!(
            tool_completion_status("terminal", Some(r#"{"exit_code": 2}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("execute_code", Some(r#"{"returncode": 1}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("skill_manage", Some(r#"{"success": false}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("some_tool", Some(r#"{"ok": false}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("read_file", Some(r#"{"error": "File not found"}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("some_tool", Some(r#"{"error": "optional timeout"}"#)),
            "completed"
        );
        assert_eq!(
            tool_completion_status("terminal", Some("Error: pytest collected 0 items")),
            "completed"
        );
        assert_eq!(
            tool_completion_status("patch", Some("Error executing tool 'patch': boom")),
            "failed"
        );
    }
}
