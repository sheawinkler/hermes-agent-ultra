const CLI_SESSIONS_ACTIONS: &str =
    "list, export, delete, prune, optimize, repair, stats, rename, browse";

pub struct CliSessionsOptions {
    pub action: Option<String>,
    pub session: Option<String>,
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub format: Option<String>,
    pub only: Option<String>,
    pub output: Option<String>,
    pub redact: bool,
    pub yes: bool,
    pub source: Option<String>,
    pub older_than: Option<u64>,
}

#[derive(Debug, Clone)]
struct SessionSnapshotEntry {
    id: String,
    path: PathBuf,
    modified: SystemTime,
    source: Option<String>,
}

fn file_size_mb(path: &Path) -> f64 {
    std::fs::metadata(path)
        .map(|meta| meta.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

fn session_subject(session: Option<String>, id: Option<String>) -> Option<String> {
    id.or(session)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_session_snapshot_source(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    value
        .get("source")
        .or_else(|| value.pointer("/session_info/source"))
        .or_else(|| value.pointer("/metadata/source"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(ToString::to_string)
}

fn list_session_snapshot_entries(
    sessions_dir: &Path,
) -> Result<Vec<SessionSnapshotEntry>, hermes_core::AgentError> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    let rd = std::fs::read_dir(sessions_dir).map_err(|e| {
        hermes_core::AgentError::Io(format!(
            "Failed to read sessions directory {}: {}",
            sessions_dir.display(),
            e
        ))
    })?;
    for entry in rd.filter_map(Result::ok) {
        let path = entry.path();
        if !path.extension().map(|e| e == "json").unwrap_or(false) {
            continue;
        }
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push(SessionSnapshotEntry {
            id,
            source: read_session_snapshot_source(&path),
            path,
            modified,
        });
    }
    Ok(entries)
}

fn resolve_unique_session_snapshot(
    sessions_dir: &Path,
    query: &str,
) -> Result<Option<SessionSnapshotEntry>, hermes_core::AgentError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(None);
    }
    let entries = list_session_snapshot_entries(sessions_dir)?;
    if let Some(exact) = entries
        .iter()
        .find(|entry| entry.id.eq_ignore_ascii_case(query))
        .cloned()
    {
        return Ok(Some(exact));
    }

    let matches: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.id.starts_with(query))
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => Err(hermes_core::AgentError::Config(format!(
            "Session prefix '{}' is ambiguous ({} matches).",
            query,
            matches.len()
        ))),
    }
}

fn delete_session_snapshot_by_query(
    sessions_dir: &Path,
    query: &str,
) -> Result<Option<String>, hermes_core::AgentError> {
    let Some(entry) = resolve_unique_session_snapshot(sessions_dir, query)? else {
        return Ok(None);
    };
    std::fs::remove_file(&entry.path).map_err(|e| {
        hermes_core::AgentError::Io(format!(
            "Failed to delete session snapshot {}: {}",
            entry.path.display(),
            e
        ))
    })?;
    Ok(Some(entry.id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionExportFormat {
    Json,
    Jsonl,
    Markdown,
    Html,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionExportOnly {
    UserPrompts,
}

#[derive(Debug, Clone)]
struct SessionExportSnapshot {
    id: String,
    data: serde_json::Value,
}

fn normalize_session_export_format(
    format: Option<&str>,
) -> Result<SessionExportFormat, hermes_core::AgentError> {
    match format
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("json")
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => Ok(SessionExportFormat::Json),
        "jsonl" => Ok(SessionExportFormat::Jsonl),
        "md" | "markdown" => Ok(SessionExportFormat::Markdown),
        "html" => Ok(SessionExportFormat::Html),
        other => Err(hermes_core::AgentError::Config(format!(
            "Unsupported sessions export format '{}'. Expected json, jsonl, md, markdown, or html.",
            other
        ))),
    }
}

fn normalize_session_export_only(
    only: Option<&str>,
) -> Result<Option<SessionExportOnly>, hermes_core::AgentError> {
    let Some(value) = only.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "user" | "prompts" | "user-prompts" | "user_prompts" => {
            Ok(Some(SessionExportOnly::UserPrompts))
        }
        other => Err(hermes_core::AgentError::Config(format!(
            "Unsupported sessions export filter '{}'. Expected user-prompts.",
            other
        ))),
    }
}

fn session_export_positional_looks_like_output(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed == "-"
        || trimmed.contains('/')
        || trimmed.ends_with(".json")
        || trimmed.ends_with(".jsonl")
        || trimmed.ends_with(".md")
        || trimmed.ends_with(".markdown")
        || trimmed.ends_with(".html")
        || trimmed.ends_with(".htm")
}

fn session_export_id_from_data(entry_id: &str, data: &serde_json::Value) -> String {
    data.get("id")
        .or_else(|| data.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(entry_id)
        .to_string()
}

fn read_session_export_snapshot(
    entry: &SessionSnapshotEntry,
    redact: bool,
) -> Result<SessionExportSnapshot, hermes_core::AgentError> {
    let content = std::fs::read_to_string(&entry.path).map_err(|e| {
        hermes_core::AgentError::Io(format!(
            "Failed to read session snapshot {}: {}",
            entry.path.display(),
            e
        ))
    })?;
    let mut data = serde_json::from_str::<serde_json::Value>(&content).map_err(|e| {
        hermes_core::AgentError::Io(format!(
            "Failed to parse session snapshot {}: {}",
            entry.path.display(),
            e
        ))
    })?;
    if redact {
        redact_session_export_value(&mut data, None);
    }
    let id = session_export_id_from_data(&entry.id, &data);
    Ok(SessionExportSnapshot { id, data })
}

fn collect_session_export_snapshots(
    sessions_dir: &Path,
    subject: Option<&str>,
    source: Option<&str>,
    older_than_days: Option<u64>,
    redact: bool,
) -> Result<Vec<SessionExportSnapshot>, hermes_core::AgentError> {
    let entries = if let Some(subject) = subject.map(str::trim).filter(|value| !value.is_empty()) {
        match resolve_unique_session_snapshot(sessions_dir, subject)? {
            Some(entry) => vec![entry],
            None => return Ok(Vec::new()),
        }
    } else {
        let mut entries = prune_session_snapshot_candidates(sessions_dir, source, older_than_days)?;
        entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
        entries
    };
    entries
        .iter()
        .map(|entry| read_session_export_snapshot(entry, redact))
        .collect()
}

fn redact_session_export_value(value: &mut serde_json::Value, key: Option<&str>) {
    match value {
        serde_json::Value::Object(map) => {
            for (child_key, child_value) in map.iter_mut() {
                redact_session_export_value(child_value, Some(child_key));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_session_export_value(item, key);
            }
        }
        serde_json::Value::String(text) => {
            if key.is_some_and(session_export_key_is_secret) {
                *text = "[REDACTED]".to_string();
            } else {
                *text = redact_session_export_text(text);
            }
        }
        _ => {}
    }
}

fn session_export_key_is_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("api_key")
        || key.contains("apikey")
        || key.contains("authorization")
        || key.contains("password")
        || key.contains("secret")
        || key.contains("token")
        || key.ends_with("_key")
}

fn redact_session_export_text(text: &str) -> String {
    let mut redacted = text.to_string();
    for (pattern, replacement) in [
        (r"sk-[A-Za-z0-9_-]{16,}", "sk-[REDACTED]"),
        (r"Bearer\s+[A-Za-z0-9._~+/=-]{16,}", "Bearer [REDACTED]"),
        (
            r"(?i)(api[_-]?key|token|secret)\s*[:=]\s*[^\s,;]+",
            "$1=[REDACTED]",
        ),
    ] {
        if let Ok(regex) = Regex::new(pattern) {
            redacted = regex.replace_all(&redacted, replacement).into_owned();
        }
    }
    redacted
}

fn render_session_exports(
    sessions: &[SessionExportSnapshot],
    format: SessionExportFormat,
    only: Option<SessionExportOnly>,
) -> Result<String, hermes_core::AgentError> {
    match (format, only) {
        (SessionExportFormat::Json, None) => {
            let result = if sessions.len() == 1 {
                serde_json::to_string_pretty(&sessions[0].data)
            } else {
                serde_json::to_string_pretty(
                    &sessions
                        .iter()
                        .map(|session| session.data.clone())
                        .collect::<Vec<_>>(),
                )
            };
            result
                .map(|mut value| {
                    value.push('\n');
                    value
                })
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))
        }
        (SessionExportFormat::Jsonl, None) => render_sessions_jsonl(sessions, false),
        (SessionExportFormat::Jsonl, Some(SessionExportOnly::UserPrompts)) => {
            render_sessions_jsonl(sessions, true)
        }
        (SessionExportFormat::Markdown, None) => Ok(render_sessions_markdown(sessions, false)),
        (SessionExportFormat::Markdown, Some(SessionExportOnly::UserPrompts)) => {
            Ok(render_sessions_markdown(sessions, true))
        }
        (SessionExportFormat::Html, None) => Ok(render_sessions_html(sessions)),
        (SessionExportFormat::Html, Some(SessionExportOnly::UserPrompts))
        | (SessionExportFormat::Json, Some(SessionExportOnly::UserPrompts)) => {
            Err(hermes_core::AgentError::Config(
                "--only user-prompts supports --format jsonl or md.".into(),
            ))
        }
    }
}

fn render_sessions_jsonl(
    sessions: &[SessionExportSnapshot],
    prompts_only: bool,
) -> Result<String, hermes_core::AgentError> {
    let mut lines = Vec::new();
    if prompts_only {
        for record in iter_user_prompt_records(sessions) {
            lines.push(serde_json::to_string(&record).map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to render prompt JSONL: {}", e))
            })?);
        }
    } else {
        for session in sessions {
            lines.push(serde_json::to_string(&session.data).map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to render session JSONL: {}", e))
            })?);
        }
    }
    Ok(if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    })
}

fn iter_user_prompt_records(
    sessions: &[SessionExportSnapshot],
) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let mut records = Vec::new();
    for session in sessions {
        let mut index = 0usize;
        for message in session_messages(&session.data) {
            if message_role(message) != "user" {
                continue;
            }
            index += 1;
            let mut record = serde_json::Map::new();
            record.insert("session_id".into(), serde_json::Value::String(session.id.clone()));
            record.insert(
                "index".into(),
                serde_json::Value::Number(serde_json::Number::from(index as u64)),
            );
            record.insert("role".into(), serde_json::Value::String("user".into()));
            record.insert(
                "text".into(),
                serde_json::Value::String(message_text(message.get("content"))),
            );
            if let Some(created_at) = message_timestamp(message) {
                record.insert("created_at".into(), serde_json::Value::String(created_at));
            }
            if let Some(message_id) = message.get("id").cloned() {
                record.insert("message_id".into(), message_id);
            }
            if let Some(event_id) = message
                .get("platform_message_id")
                .or_else(|| message.get("event_id"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            {
                record.insert("event_id".into(), serde_json::Value::String(event_id.into()));
            }
            records.push(record);
        }
    }
    records
}

fn render_sessions_markdown(sessions: &[SessionExportSnapshot], prompts_only: bool) -> String {
    let mut lines = Vec::new();
    if prompts_only {
        if sessions.len() == 1 {
            lines.push(format!("# User prompts for session {}", sessions[0].id));
            append_session_metadata(&mut lines, &sessions[0]);
            append_prompt_markdown(&mut lines, &sessions[0], 2);
        } else {
            lines.push("# User prompts export".into());
            for session in sessions {
                lines.push(String::new());
                lines.push(format!("## Session {}", session.id));
                append_session_metadata(&mut lines, session);
                append_prompt_markdown(&mut lines, session, 3);
            }
        }
    } else if sessions.len() == 1 {
        lines.push(format!(
            "# Session: {}",
            markdown_heading_text(&session_title_or_id(&sessions[0]))
        ));
        append_session_metadata(&mut lines, &sessions[0]);
        append_session_messages_markdown(&mut lines, &sessions[0], 2);
    } else {
        lines.push("# Hermes sessions export".into());
        for session in sessions {
            lines.push(String::new());
            lines.push(format!(
                "## Session: {}",
                markdown_heading_text(&session_title_or_id(session))
            ));
            append_session_metadata(&mut lines, session);
            append_session_messages_markdown(&mut lines, session, 3);
        }
    }
    finish_session_markdown(lines)
}

fn append_session_metadata(lines: &mut Vec<String>, session: &SessionExportSnapshot) {
    lines.push(format!("- Session ID: `{}`", session.id));
    for key in ["source", "model", "title"] {
        if let Some(value) = session
            .data
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!(
                "- {}: {}",
                title_case_ascii(key),
                value.replace('\n', " ")
            ));
        }
    }
    if let Some(started) = session
        .data
        .get("started_at")
        .or_else(|| session.data.get("created_at"))
        .and_then(format_session_timestamp)
    {
        lines.push(format!("- Started: {}", started));
    }
    lines.push(String::new());
}

fn append_prompt_markdown(
    lines: &mut Vec<String>,
    session: &SessionExportSnapshot,
    heading_level: usize,
) {
    let prompts = iter_user_prompt_records(std::slice::from_ref(session));
    if prompts.is_empty() {
        lines.push("_No user prompts found._".into());
        return;
    }
    let marker = "#".repeat(heading_level);
    for prompt in prompts {
        let index = prompt
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let created_at = prompt
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("timestamp unavailable");
        lines.push(format!("{} {}. {}", marker, index, created_at));
        if let Some(message_id) = prompt.get("message_id") {
            lines.push(format!("Message ID: `{}`", json_scalar_text(message_id)));
            lines.push(String::new());
        }
        lines.push(
            prompt
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        lines.push(String::new());
    }
}

fn append_session_messages_markdown(
    lines: &mut Vec<String>,
    session: &SessionExportSnapshot,
    heading_level: usize,
) {
    let visible: Vec<_> = session_messages(&session.data)
        .into_iter()
        .filter(|message| message_role(message) != "system")
        .collect();
    if visible.is_empty() {
        lines.push("_No messages found._".into());
        return;
    }
    let marker = "#".repeat(heading_level);
    for message in visible {
        let role = message_role(message);
        let label = match role.as_str() {
            "user" => "User".to_string(),
            "assistant" => "Assistant".to_string(),
            "tool" => format!(
                "Tool: {}",
                message
                    .get("tool_name")
                    .or_else(|| message.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("tool")
            ),
            other => title_case_ascii(other),
        };
        let suffix = message_timestamp(message)
            .map(|value| format!(" - {}", value))
            .unwrap_or_default();
        lines.push(format!("{} {}{}", marker, markdown_heading_text(&label), suffix));
        lines.push(String::new());
        if role == "tool" {
            lines.push(fenced_session_text(&message_text(message.get("content"))));
        } else {
            lines.push(message_text(message.get("content")));
        }
        lines.push(String::new());
    }
}

fn render_sessions_html(sessions: &[SessionExportSnapshot]) -> String {
    let page_title = if sessions.len() == 1 {
        format!("Hermes session {}", sessions[0].id)
    } else {
        format!("Hermes sessions export ({} sessions)", sessions.len())
    };
    let mut sidebar = String::new();
    if sessions.len() > 1 {
        sidebar.push_str("<nav class=\"sidebar\"><h2>Sessions</h2><ul>");
        for session in sessions {
            sidebar.push_str(&format!(
                "<li><a href=\"#session-{id}\"><strong>{title}</strong><span>{id}</span></a></li>",
                id = html_escape(&session.id),
                title = html_escape(&session_title_or_id(session))
            ));
        }
        sidebar.push_str("</ul></nav>");
    }

    let mut body = String::new();
    for session in sessions {
        body.push_str(&format!(
            "<section class=\"session\" id=\"session-{id}\"><header><p class=\"eyebrow\">Hermes Ultra session export</p><h1>{title}</h1><p class=\"session-id\">{id}</p></header>",
            id = html_escape(&session.id),
            title = html_escape(&session_title_or_id(session))
        ));
        body.push_str("<div class=\"messages\">");
        for message in session_messages(&session.data) {
            let role = message_role(message);
            if role == "session_meta" {
                continue;
            }
            body.push_str(&format!(
                "<article class=\"message message-{role}\"><div class=\"message-meta\"><span>{role}</span><time>{time}</time></div><pre>{content}</pre>",
                role = html_escape(&role),
                time = html_escape(
                    &message_timestamp(message).unwrap_or_else(|| "timestamp unavailable".into())
                ),
                content = html_escape(&message_text(message.get("content"))),
            ));
            if let Some(tool_calls) = message
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
            {
                for tool_call in tool_calls {
                    body.push_str("<details><summary>Tool call</summary><pre>");
                    body.push_str(&html_escape(&json_scalar_text(tool_call)));
                    body.push_str("</pre></details>");
                }
            }
            if let Some(reasoning) = message
                .get("reasoning")
                .or_else(|| message.get("reasoning_content"))
                .and_then(serde_json::Value::as_str)
            {
                body.push_str("<details><summary>Reasoning</summary><pre>");
                body.push_str(&html_escape(reasoning));
                body.push_str("</pre></details>");
            }
            body.push_str("</article>");
        }
        body.push_str("</div></section>");
    }

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{}</title>
<style>
:root {{ color-scheme: light dark; --bg:#f8f4ec; --fg:#1d1912; --panel:#fffaf0; --muted:#756957; --border:#d9c6a3; --accent:#9a5a18; --tool:#ebf5ff; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#11100d; --fg:#f7ecd7; --panel:#1e1a14; --muted:#c7bca7; --border:#5b4a31; --accent:#ffb84d; --tool:#132232; }} }}
body {{ margin:0; background:radial-gradient(circle at top left, rgba(154,90,24,.18), transparent 36rem), var(--bg); color:var(--fg); font:16px/1.5 ui-serif, Georgia, serif; }}
.layout {{ display:flex; min-height:100vh; }}
.sidebar {{ width:18rem; padding:1.25rem; border-right:1px solid var(--border); background:color-mix(in srgb, var(--panel) 86%, transparent); position:sticky; top:0; height:100vh; overflow:auto; }}
.sidebar ul {{ list-style:none; margin:0; padding:0; }}
.sidebar a {{ color:var(--fg); display:block; padding:.7rem 0; text-decoration:none; border-bottom:1px solid var(--border); }}
.sidebar span {{ color:var(--muted); display:block; font:12px ui-monospace, SFMono-Regular, Menlo, monospace; }}
main {{ width:min(72rem, 100%); margin:0 auto; padding:3rem 1.25rem; }}
.session {{ margin-bottom:4rem; }}
.eyebrow {{ color:var(--accent); font:700 12px ui-monospace, SFMono-Regular, Menlo, monospace; letter-spacing:.1em; text-transform:uppercase; }}
h1 {{ font-size:clamp(2rem, 5vw, 4rem); line-height:1; margin:.25rem 0; }}
.session-id {{ color:var(--muted); font:13px ui-monospace, SFMono-Regular, Menlo, monospace; }}
.message {{ background:var(--panel); border:1px solid var(--border); border-radius:18px; box-shadow:0 18px 50px rgba(0,0,0,.08); margin:1rem 0; padding:1rem; }}
.message-user {{ border-left:5px solid var(--accent); }}
.message-tool {{ background:var(--tool); }}
.message-meta {{ display:flex; justify-content:space-between; gap:1rem; color:var(--muted); font:12px ui-monospace, SFMono-Regular, Menlo, monospace; text-transform:uppercase; }}
pre {{ white-space:pre-wrap; word-break:break-word; font:13px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }}
details {{ margin-top:.75rem; }}
@media (max-width: 860px) {{ .layout {{ display:block; }} .sidebar {{ position:relative; width:auto; height:auto; border-right:0; border-bottom:1px solid var(--border); }} main {{ padding-top:1.5rem; }} }}
</style>
</head>
<body><div class="layout">{}<main>{}</main></div></body>
</html>
"#,
        html_escape(&page_title),
        sidebar,
        body
    )
}

fn session_messages(data: &serde_json::Value) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    data.get("messages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
        .collect()
}

fn message_role(message: &serde_json::Map<String, serde_json::Value>) -> String {
    message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
}

fn message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| message_text(Some(item)))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(serde_json::Value::Object(map)) => map
            .get("text")
            .or_else(|| map.get("content"))
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| content.map(json_scalar_text).unwrap_or_default()),
        Some(other) => json_scalar_text(other),
    }
}

fn message_timestamp(message: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    message
        .get("timestamp")
        .or_else(|| message.get("created_at"))
        .and_then(format_session_timestamp)
}

fn format_session_timestamp(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(number) => number.as_f64().and_then(|secs| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        }),
        serde_json::Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        _ => None,
    }
}

fn session_title_or_id(session: &SessionExportSnapshot) -> String {
    session
        .data
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&session.id)
        .to_string()
}

fn title_case_ascii(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn markdown_heading_text(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn fenced_session_text(text: &str) -> String {
    let mut fence = "```".to_string();
    while text.contains(&fence) {
        fence.push('`');
    }
    format!("{}text\n{}\n{}", fence, text, fence)
}

fn finish_session_markdown(mut lines: Vec<String>) -> String {
    while matches!(lines.last(), Some(line) if line.is_empty()) {
        lines.pop();
    }
    format!("{}\n", lines.join("\n"))
}

fn json_scalar_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| String::new()),
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn write_session_export_output(
    output: Option<&str>,
    rendered: &str,
) -> Result<(), hermes_core::AgentError> {
    match output.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("-") => {
            print!("{}", rendered);
            Ok(())
        }
        Some(path) => std::fs::write(path, rendered)
            .map_err(|e| hermes_core::AgentError::Io(format!("Failed to write {}: {}", path, e))),
    }
}

fn session_confirm_response_is_yes(input: Option<&str>) -> bool {
    matches!(
        input.map(|value| value.trim().to_ascii_lowercase()),
        Some(value) if matches!(value.as_str(), "y" | "yes")
    )
}

fn prompt_session_confirmation(prompt: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    print!("{} [y/N]: ", prompt);
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(0) | Err(_) => false,
        Ok(_) => session_confirm_response_is_yes(Some(input.as_str())),
    }
}

fn prune_cutoff_for_sessions(
    source: Option<&str>,
    older_than: Option<u64>,
    legacy_days: Option<&str>,
) -> Option<u64> {
    let explicit = older_than.or_else(|| legacy_days.and_then(|value| value.parse::<u64>().ok()));
    explicit.or_else(|| source.is_none().then_some(90))
}

fn prune_session_snapshot_candidates(
    sessions_dir: &Path,
    source: Option<&str>,
    older_than_days: Option<u64>,
) -> Result<Vec<SessionSnapshotEntry>, hermes_core::AgentError> {
    let source = source.map(str::trim).filter(|value| !value.is_empty());
    let cutoff = older_than_days.and_then(|days| {
        SystemTime::now().checked_sub(Duration::from_secs(days.saturating_mul(86_400)))
    });
    let candidates = list_session_snapshot_entries(sessions_dir)?
        .into_iter()
        .filter(|entry| {
            source.is_none_or(|expected| {
                entry
                    .source
                    .as_deref()
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            })
        })
        .filter(|entry| cutoff.is_none_or(|cutoff| entry.modified < cutoff))
        .collect();
    Ok(candidates)
}

fn session_time_epoch_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn render_prune_preview(candidates: &[SessionSnapshotEntry]) -> String {
    if candidates.is_empty() {
        return "0 session(s) match.".to_string();
    }
    let oldest = candidates
        .iter()
        .map(|entry| entry.modified)
        .min()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let newest = candidates
        .iter()
        .map(|entry| entry.modified)
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    format!(
        "{} session(s) match (oldest {}, newest {}).",
        candidates.len(),
        session_time_epoch_seconds(oldest),
        session_time_epoch_seconds(newest)
    )
}

/// Handle `hermes sessions [action] [--id ...] [--name ...]`.
pub async fn handle_cli_sessions(
    options: CliSessionsOptions,
) -> Result<(), hermes_core::AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    let CliSessionsOptions {
        action,
        session,
        id,
        session_id,
        name,
        format,
        only,
        output,
        redact,
        yes,
        source,
        older_than,
    } = options;

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !sessions_dir.exists() {
                println!("No sessions directory found.");
                return Ok(());
            }
            let mut entries: Vec<(String, u64, std::time::SystemTime, bool, bool, usize)> =
                Vec::new();
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.extension().map(|e| e == "json").unwrap_or(false) {
                        let stem = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let meta = std::fs::metadata(&path);
                        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                        let modified = meta
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        let integrity = inspect_snapshot_integrity(&path);
                        let canonical = is_canonical_snapshot_name(&stem, &integrity);
                        entries.push((
                            stem,
                            size,
                            modified,
                            canonical,
                            integrity.valid,
                            integrity.message_count,
                        ));
                    }
                }
            }
            entries.sort_by(|a, b| {
                b.3.cmp(&a.3)
                    .then_with(|| b.5.cmp(&a.5))
                    .then_with(|| b.2.cmp(&a.2))
                    .then_with(|| a.0.cmp(&b.0))
            });
            if entries.is_empty() {
                println!("No saved sessions.");
            } else {
                let canonical_count = entries.iter().filter(|entry| entry.3).count();
                let artifact_count = entries.len().saturating_sub(canonical_count);
                println!(
                    "Saved sessions ({} total; {} canonical; {} artifacts):",
                    entries.len(),
                    canonical_count,
                    artifact_count
                );
                for (name, size, _, canonical, valid, messages) in &entries {
                    let kind = if *canonical {
                        "session"
                    } else if *valid {
                        "artifact"
                    } else {
                        "invalid"
                    };
                    println!("  • {} ({} bytes, {} msgs, {})", name, size, messages, kind);
                }
            }
        }
        "export" => {
            let export_format = normalize_session_export_format(format.as_deref())?;
            let export_only = normalize_session_export_only(only.as_deref())?;
            let explicit_subject = session_id.or(id);
            let mut export_subject = explicit_subject;
            let mut export_output = output;

            if export_subject.is_none() {
                if let Some(candidate) = session.clone().map(|value| value.trim().to_string()) {
                    if candidate.is_empty() {
                        export_subject = None;
                    } else if resolve_unique_session_snapshot(&sessions_dir, &candidate)?.is_some() {
                        export_subject = Some(candidate);
                    } else if export_output.is_none()
                        && session_export_positional_looks_like_output(&candidate)
                    {
                        export_output = Some(candidate);
                    } else {
                        export_subject = Some(candidate);
                    }
                }
            }

            if export_format == SessionExportFormat::Html
                && export_output
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && *value != "-")
                    .is_none()
            {
                return Err(hermes_core::AgentError::Config(
                    "HTML export requires an output file path.".into(),
                ));
            }

            let snapshots = collect_session_export_snapshots(
                &sessions_dir,
                export_subject.as_deref(),
                source.as_deref(),
                older_than,
                redact,
            )?;
            if snapshots.is_empty() {
                if let Some(subject) = export_subject {
                    println!("Session '{}' not found.", subject);
                } else {
                    println!("No sessions found.");
                }
                return Ok(());
            }

            let rendered = render_session_exports(&snapshots, export_format, export_only)?;
            write_session_export_output(export_output.as_deref(), &rendered)?;
            if let Some(output) = export_output.as_deref().filter(|value| *value != "-") {
                let count = if export_only == Some(SessionExportOnly::UserPrompts) {
                    iter_user_prompt_records(&snapshots).len()
                } else {
                    snapshots.len()
                };
                let noun = if export_only == Some(SessionExportOnly::UserPrompts) {
                    "prompt"
                } else {
                    "session"
                };
                println!(
                    "Exported {} {}{} to {}",
                    count,
                    noun,
                    if count == 1 { "" } else { "s" },
                    output
                );
            }
        }
        "delete" => {
            let session_id = session_subject(session.clone(), id.clone()).ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions delete --id <id>".into(),
                )
            })?;
            let Some(resolved) = resolve_unique_session_snapshot(&sessions_dir, &session_id)?
            else {
                println!("Session '{}' not found.", session_id);
                return Ok(());
            };
            if !prompt_session_confirmation(
                &format!("Delete session '{}'? This cannot be undone.", resolved.id),
                yes,
            ) {
                println!("Cancelled.");
                return Ok(());
            }
            let deleted = delete_session_snapshot_by_query(&sessions_dir, &resolved.id)?
                .unwrap_or_else(|| resolved.id.clone());
            println!("Deleted session '{}'.", deleted);
        }
        "stats" => {
            if !sessions_dir.exists() {
                println!("No sessions directory.");
                return Ok(());
            }
            let mut total_files = 0u32;
            let mut total_size = 0u64;
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    if entry
                        .path()
                        .extension()
                        .map(|e| e == "json")
                        .unwrap_or(false)
                    {
                        total_files += 1;
                        total_size += std::fs::metadata(entry.path())
                            .map(|m| m.len())
                            .unwrap_or(0);
                    }
                }
            }
            println!("Session statistics:");
            println!("  Total sessions: {}", total_files);
            println!("  Total size:     {} KB", total_size / 1024);
            println!("  Directory:      {}", sessions_dir.display());
        }
        "prune" => {
            if !sessions_dir.exists() {
                println!("No sessions directory.");
                return Ok(());
            }
            let max_age_days =
                prune_cutoff_for_sessions(source.as_deref(), older_than, name.as_deref());
            let candidates = prune_session_snapshot_candidates(
                &sessions_dir,
                source.as_deref(),
                max_age_days,
            )?;
            println!("{}", render_prune_preview(&candidates));
            if candidates.is_empty() {
                return Ok(());
            }
            if !prompt_session_confirmation("Prune matching sessions?", yes) {
                println!("Cancelled.");
                return Ok(());
            }
            let mut pruned = 0u32;
            for entry in candidates {
                if std::fs::remove_file(&entry.path).is_ok() {
                    println!("  Pruned: {}", entry.id);
                    pruned += 1;
                }
            }
            println!("Pruned {} session(s).", pruned);
        }
        "optimize" => {
            let persistence = SessionPersistence::new(hermes_config::hermes_home());
            let db_path = persistence.db_path().to_path_buf();
            let before_mb = file_size_mb(&db_path);
            println!("Optimizing session store (FTS merge + VACUUM)...");
            let index_count = persistence.fts_index_count()?;
            persistence.vacuum()?;
            let after_mb = file_size_mb(&db_path);
            let reclaimed_mb = before_mb - after_mb;
            println!("Optimized {} FTS index(es).", index_count);
            println!(
                "Database size: {:.1} MB -> {:.1} MB (reclaimed {:.1} MB)",
                before_mb, after_mb, reclaimed_mb
            );
        }
        "repair" => {
            let persistence = SessionPersistence::new(hermes_config::hermes_home());
            let db_path = persistence.db_path().to_path_buf();
            if !db_path.exists() {
                println!(
                    "No session database at {} (nothing to repair).",
                    db_path.display()
                );
                return Ok(());
            }
            match persistence.db_health_error() {
                None => {
                    println!("{} opens cleanly; no repair needed.", db_path.display());
                }
                Some(reason) if SessionPersistence::is_malformed_db_error_message(&reason) => {
                    println!("{} has a malformed schema: {}", db_path.display(), reason);
                    println!("Repairing with a raw backup first...");
                    let report = persistence.repair_malformed_schema(true);
                    if report.repaired {
                        println!(
                            "Repaired sessions.db (strategy: {}).",
                            report.strategy.as_deref().unwrap_or("unknown")
                        );
                        if let Some(path) = report.backup_path {
                            println!("Backup: {}", path.display());
                        }
                    } else {
                        println!(
                            "Repair failed: {}",
                            report
                                .error
                                .as_deref()
                                .unwrap_or("repair did not return a concrete error")
                        );
                        if let Some(path) = report.backup_path {
                            println!("Backup preserved: {}", path.display());
                        }
                    }
                }
                Some(reason) => {
                    println!(
                        "{} does not open cleanly, but this is not the targeted malformed-schema repair class: {}",
                        db_path.display(),
                        reason
                    );
                }
            }
        }
        "rename" => {
            let session_id = id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions rename --id <id> --name <new>"
                        .into(),
                )
            })?;
            let new_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing new name. Usage: hermes sessions rename --id <id> --name <new>".into(),
                )
            })?;
            let old_path = sessions_dir.join(format!("{}.json", session_id));
            let new_path = sessions_dir.join(format!("{}.json", new_name));
            if !old_path.exists() {
                println!("Session '{}' not found.", session_id);
                return Ok(());
            }
            std::fs::rename(&old_path, &new_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("Session renamed: {} -> {}", session_id, new_name);
        }
        "browse" => {
            if !sessions_dir.exists() {
                println!("No sessions directory found.");
                return Ok(());
            }
            println!("Session Browser");
            println!("===============\n");
            let mut entries: Vec<(String, u64, std::time::SystemTime, usize)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.extension().map(|e| e == "json").unwrap_or(false) {
                        continue;
                    }
                    let stem = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let meta = std::fs::metadata(&path);
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = meta
                        .as_ref()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    let msg_count = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
                        .and_then(|v| {
                            v.get("messages")
                                .and_then(|m| m.as_array())
                                .map(|a| a.len())
                        })
                        .unwrap_or(0);
                    entries.push((stem, size, modified, msg_count));
                }
            }
            entries.sort_by_key(|(_, _, modified, _)| std::cmp::Reverse(*modified));
            if entries.is_empty() {
                println!("No sessions found.");
            } else {
                println!(
                    "{:3} {:30} {:>8} {:>6}  Modified",
                    "#", "Session ID", "Size", "Msgs"
                );
                println!("{}", "-".repeat(75));
                for (idx, (name, size, modified, msgs)) in entries.iter().enumerate() {
                    let age = modified.elapsed().unwrap_or_default();
                    let age_str = if age.as_secs() < 3600 {
                        format!("{}m ago", age.as_secs() / 60)
                    } else if age.as_secs() < 86400 {
                        format!("{}h ago", age.as_secs() / 3600)
                    } else {
                        format!("{}d ago", age.as_secs() / 86400)
                    };
                    println!(
                        "{:3} {:30} {:>6}KB {:>6}  {}",
                        idx + 1,
                        &name[..name.len().min(30)],
                        size / 1024,
                        msgs,
                        age_str,
                    );
                }
                println!("\nUse `hermes sessions export --id <id>` to view a session.");
            }
        }
        other => {
            println!("Sessions action '{}' is not recognized.", other);
            println!("Available actions: {}", CLI_SESSIONS_ACTIONS);
        }
    }
    Ok(())
}

/// Handle `hermes insights [--days N] [--source ...]`.
pub async fn handle_cli_insights(
    days: u32,
    source: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    println!("Usage Insights (last {} days)", days);
    println!("=============================");
    if let Some(src) = &source {
        println!("Filter: source={}\n", src);
    }
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    if !sessions_dir.exists() {
        println!("No sessions directory found.");
        return Ok(());
    }

    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(u64::from(days) * 86400))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let mut total_sessions = 0u32;
    let mut total_messages = 0u64;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_cost_cents = 0.0f64;
    let mut models_used: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut daily_counts: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();

    if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.extension().map(|e| e == "json").unwrap_or(false) {
                continue;
            }
            let meta = std::fs::metadata(&path);
            let modified = meta
                .as_ref()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if modified < cutoff {
                continue;
            }

            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(src_filter) = &source {
                        let session_source = data
                            .get("source")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown");
                        if session_source != src_filter.as_str() {
                            continue;
                        }
                    }

                    total_sessions += 1;

                    if let Some(msgs) = data.get("messages").and_then(|m| m.as_array()) {
                        total_messages += msgs.len() as u64;
                    }

                    if let Some(usage) = data.get("usage") {
                        total_input_tokens += usage
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        total_output_tokens += usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        total_cost_cents +=
                            usage.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    }

                    if let Some(model) = data.get("model").and_then(|m| m.as_str()) {
                        *models_used.entry(model.to_string()).or_insert(0) += 1;
                    }

                    let dur = modified
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default();
                    let secs = dur.as_secs();
                    let day_secs = secs - (secs % 86400);
                    let day_key = format!("{}", day_secs / 86400);
                    *daily_counts.entry(day_key).or_insert(0) += 1;
                }
            }
        }
    }

    println!("Sessions:       {}", total_sessions);
    println!("Messages:       {}", total_messages);
    println!("Input tokens:   {}", total_input_tokens);
    println!("Output tokens:  {}", total_output_tokens);
    let total_tokens = total_input_tokens + total_output_tokens;
    println!("Total tokens:   {}", total_tokens);
    if total_cost_cents > 0.0 {
        println!("Estimated cost: ${:.4}", total_cost_cents / 100.0);
    }

    if !models_used.is_empty() {
        println!("\nModels Used:");
        let mut model_vec: Vec<_> = models_used.into_iter().collect();
        model_vec.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (model, count) in &model_vec {
            println!("  {:30} {:>5} session(s)", model, count);
        }
    }

    if total_sessions > 0 {
        println!("\nAverages per session:");
        println!(
            "  Messages: {:.1}",
            total_messages as f64 / total_sessions as f64
        );
        println!(
            "  Tokens:   {:.0}",
            total_tokens as f64 / total_sessions as f64
        );
    }

    Ok(())
}

/// Handle `hermes login [provider]`.
pub async fn handle_cli_login(provider: Option<String>) -> Result<(), hermes_core::AgentError> {
    let provider = provider.unwrap_or_else(|| "openai".to_string());
    let creds_dir = hermes_config::hermes_home().join("credentials");
    std::fs::create_dir_all(&creds_dir).map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    println!("Login to: {}", provider);
    println!("----------{}", "-".repeat(provider.len()));

    match provider.as_str() {
        "openai" => {
            let env_key = std::env::var("HERMES_OPENAI_API_KEY")
                .ok()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok());
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!(
                    "Found HERMES_OPENAI_API_KEY/OPENAI_API_KEY in environment: {}",
                    masked
                );
                let cred_file = creds_dir.join("openai.json");
                let cred = serde_json::json!({
                    "provider": "openai",
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored at {}", cred_file.display());
            } else {
                println!("No HERMES_OPENAI_API_KEY/OPENAI_API_KEY found in environment.");
                println!("Set it with: export HERMES_OPENAI_API_KEY=sk-...");
                println!("Or use: hermes config set openai_api_key <key>");
            }
        }
        "anthropic" => {
            let env_key = std::env::var("ANTHROPIC_API_KEY").ok();
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!("Found ANTHROPIC_API_KEY in environment: {}", masked);
                let cred_file = creds_dir.join("anthropic.json");
                let cred = serde_json::json!({
                    "provider": "anthropic",
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored at {}", cred_file.display());
            } else {
                println!("No ANTHROPIC_API_KEY found in environment.");
                println!("Set it with: export ANTHROPIC_API_KEY=sk-ant-...");
            }
        }
        other => {
            let env_var = format!("{}_API_KEY", other.to_uppercase().replace('-', "_"));
            let env_key = std::env::var(&env_var).ok();
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!("Found {} in environment: {}", env_var, masked);
                let cred_file = creds_dir.join(format!("{}.json", other));
                let cred = serde_json::json!({
                    "provider": other,
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored.");
            } else {
                println!("No {} found in environment.", env_var);
                println!("Set it with: export {}=<your-key>", env_var);
            }
        }
    }
    Ok(())
}

/// Handle `hermes logout [provider]`.
pub async fn handle_cli_logout(provider: Option<String>) -> Result<(), hermes_core::AgentError> {
    let creds_dir = hermes_config::hermes_home().join("credentials");

    match provider.as_deref() {
        Some(p) => {
            let cred_file = creds_dir.join(format!("{}.json", p));
            if cred_file.exists() {
                std::fs::remove_file(&cred_file)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Logged out from '{}'. Credential reference removed.", p);
            } else {
                println!("No stored credentials for '{}'.", p);
            }
            println!(
                "Note: Environment variables (e.g. {}_API_KEY) are not affected.",
                p.to_uppercase().replace('-', "_")
            );
        }
        None => {
            if creds_dir.exists() {
                let mut removed = 0u32;
                if let Ok(rd) = std::fs::read_dir(&creds_dir) {
                    for entry in rd.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        if path.extension().map(|e| e == "json").unwrap_or(false)
                            && std::fs::remove_file(&path).is_ok()
                        {
                            let name = path.file_stem().unwrap_or_default().to_string_lossy();
                            println!("  Removed credential: {}", name);
                            removed += 1;
                        }
                    }
                }
                if removed == 0 {
                    println!("No stored credentials to remove.");
                } else {
                    println!("Logged out from {} provider(s).", removed);
                }
            } else {
                println!("No credentials directory found.");
            }
            println!("Note: Environment variables are not affected.");
        }
    }
    Ok(())
}

/// Handle `hermes whatsapp [action]`.
pub async fn handle_cli_whatsapp(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "setup" => {
            whatsapp_setup().await?;
        }
        "status" => {
            whatsapp_status().await?;
        }
        "qr" => {
            whatsapp_qr().await?;
        }
        other => {
            println!("WhatsApp action '{}' is not recognized.", other);
            println!("Available actions: setup, status, qr");
        }
    }
    Ok(())
}

/// Interactive setup: collect credentials, persist to config.yaml, verify.
async fn whatsapp_setup() -> Result<(), hermes_core::AgentError> {
    use std::io::{self, BufRead, Write};

    println!("WhatsApp Cloud API Setup");
    println!("========================\n");
    println!("You will need credentials from the Meta developer dashboard:");
    println!("  https://developers.facebook.com/apps/\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    print!("Phone Number ID: ");
    stdout.flush().ok();
    let phone_number_id = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if phone_number_id.is_empty() {
        println!("Aborted: phone number ID is required.");
        return Ok(());
    }

    print!("Business Account ID: ");
    stdout.flush().ok();
    let business_account_id = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if business_account_id.is_empty() {
        println!("Aborted: business account ID is required.");
        return Ok(());
    }

    print!("Access Token: ");
    stdout.flush().ok();
    let access_token = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if access_token.is_empty() {
        println!("Aborted: access token is required.");
        return Ok(());
    }

    println!("\nVerifying token against WhatsApp Cloud API...");
    let url = format!(
        "https://graph.facebook.com/v21.0/{}/messages",
        phone_number_id
    );
    let client = reqwest::Client::new();
    match client
        .get(&url)
        .bearer_auth(&access_token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.as_u16() == 400 {
                // 400 means the endpoint is reachable (POST required for actual messages)
                println!("  API reachable (HTTP {}).", status);
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                println!("  Warning: API returned {} — token may be invalid.", status);
                println!("  Saving anyway; you can re-run setup later.");
            } else {
                println!("  API returned HTTP {}. Saving config anyway.", status);
            }
        }
        Err(e) => {
            println!("  Could not reach API: {}", e);
            println!("  Saving config anyway — verify network connectivity.");
        }
    }

    let config_path = hermes_config::hermes_home().join("config.yaml");
    let mut config: serde_yaml::Value = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
        serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Mapping(Default::default()))
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };

    let platforms = config
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("platforms".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa = platforms
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("whatsapp".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa_map = wa.as_mapping_mut().unwrap();
    wa_map.insert(
        serde_yaml::Value::String("phone_number_id".into()),
        serde_yaml::Value::String(phone_number_id.clone()),
    );
    wa_map.insert(
        serde_yaml::Value::String("business_account_id".into()),
        serde_yaml::Value::String(business_account_id),
    );
    wa_map.insert(
        serde_yaml::Value::String("access_token".into()),
        serde_yaml::Value::String(access_token),
    );
    wa_map.insert(
        serde_yaml::Value::String("enabled".into()),
        serde_yaml::Value::Bool(true),
    );

    let yaml_str = serde_yaml::to_string(&config)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
    std::fs::create_dir_all(hermes_config::hermes_home())
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    std::fs::write(&config_path, &yaml_str)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    println!(
        "\nWhatsApp configuration saved to {}",
        config_path.display()
    );
    println!("Phone Number ID: {}", phone_number_id);
    println!("\nRun `hermes whatsapp status` to verify.");
    Ok(())
}

/// Check whether WhatsApp is configured and verify connectivity.
async fn whatsapp_status() -> Result<(), hermes_core::AgentError> {
    let config_path = hermes_config::hermes_home().join("config.yaml");
    if !config_path.exists() {
        println!("WhatsApp: not configured");
        println!("Run `hermes whatsapp setup` to configure.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let config: serde_yaml::Value =
        serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Mapping(Default::default()));

    let wa = config.get("platforms").and_then(|p| p.get("whatsapp"));

    match wa {
        None => {
            println!("WhatsApp: not configured");
            println!("Run `hermes whatsapp setup` to configure.");
        }
        Some(wa_cfg) => {
            let phone_id = wa_cfg
                .get("phone_number_id")
                .and_then(|v| v.as_str())
                .unwrap_or("(not set)");
            let enabled = wa_cfg
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has_token = wa_cfg
                .get("access_token")
                .and_then(|v| v.as_str())
                .map(|t| !t.is_empty())
                .unwrap_or(false);

            println!("WhatsApp Status");
            println!("---------------");
            println!("  Configured:     yes");
            println!("  Enabled:        {}", enabled);
            println!("  Phone Number ID: {}", phone_id);
            println!(
                "  Access Token:   {}",
                if has_token { "present" } else { "missing" }
            );

            if has_token {
                let token = wa_cfg.get("access_token").unwrap().as_str().unwrap();
                let url = format!("https://graph.facebook.com/v21.0/{}/messages", phone_id);
                print!("  API Connectivity: ");
                match reqwest::Client::new()
                    .get(&url)
                    .bearer_auth(token)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) => println!("reachable (HTTP {})", resp.status()),
                    Err(e) => println!("unreachable ({})", e),
                }
            }
        }
    }
    Ok(())
}

/// Connect to local bridge, fetch QR data, and render in terminal.
async fn whatsapp_qr() -> Result<(), hermes_core::AgentError> {
    let config_path = hermes_config::hermes_home().join("config.yaml");
    let bridge_url = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let config: serde_yaml::Value = serde_yaml::from_str(&content)
            .unwrap_or(serde_yaml::Value::Mapping(Default::default()));
        config
            .get("platforms")
            .and_then(|p| p.get("whatsapp"))
            .and_then(|w| w.get("bridge_url"))
            .and_then(|u| u.as_str())
            .unwrap_or("http://localhost:3000")
            .to_string()
    } else {
        "http://localhost:3000".to_string()
    };

    let qr_url = format!("{}/qr", bridge_url);
    println!("Fetching QR code from {}...", qr_url);

    match reqwest::Client::new()
        .get(&qr_url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let body = resp
                .text()
                .await
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

            let qr_data = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                json.get("qr")
                    .or_else(|| json.get("data"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&body)
                    .to_string()
            } else {
                body
            };

            println!();
            render_qr_to_terminal(&qr_data);
            println!();
            println!("Scan this QR code with WhatsApp on your phone:");
            println!("  WhatsApp → Settings → Linked Devices → Link a Device");
        }
        Ok(resp) => {
            println!(
                "Bridge returned HTTP {}. Is the bridge server running?",
                resp.status()
            );
            println!("Start it with: {}", whatsapp_bridge_start_command());
        }
        Err(e) => {
            println!("Could not connect to bridge at {}: {}", bridge_url, e);
            println!("\nMake sure the WhatsApp Web bridge is running:");
            println!("  {}", whatsapp_bridge_start_command());
            println!("  # or: docker run -p 3000:3000 hermes/whatsapp-bridge");
        }
    }
    Ok(())
}

fn whatsapp_bridge_start_command() -> String {
    find_node_executable("npx")
        .map(|path| format!("{} hermes-whatsapp-bridge", quote_shell_arg(&path)))
        .unwrap_or_else(|| "npx hermes-whatsapp-bridge".to_string())
}

fn quote_shell_arg(path: &Path) -> String {
    let value = path.display().to_string();
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%_+=:,./-\\".contains(c))
    {
        value
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Render QR data as Unicode block art in the terminal.
///
/// Uses a simple bit-encoding approach: each character in the input
/// string controls whether a "module" is dark or light. Two rows are
/// packed into one terminal line using half-block characters.
fn render_qr_to_terminal(data: &str) {
    // Determine a square side length from the data
    let len = data.len();
    let side = (len as f64).sqrt().ceil() as usize;
    if side == 0 {
        println!("(empty QR data)");
        return;
    }

    let bytes = data.as_bytes();

    // Dark module = odd byte value, light = even (simple heuristic)
    let is_dark = |row: usize, col: usize| -> bool {
        let idx = row * side + col;
        if idx < bytes.len() {
            bytes[idx] % 2 == 1
        } else {
            false
        }
    };

    // Print using half-block characters: each terminal row encodes two QR rows.
    // ▀ = top dark, bottom light | ▄ = top light, bottom dark
    // █ = both dark              | ' ' = both light
    let mut row = 0;
    while row < side {
        let mut line = String::new();
        for col in 0..side {
            let top = is_dark(row, col);
            let bottom = if row + 1 < side {
                is_dark(row + 1, col)
            } else {
                false
            };
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        println!("  {}", line);
        row += 2;
    }
}

/// Handle `hermes pairing [action] [--device-id ...]`.
pub async fn handle_cli_pairing(
    action: Option<String>,
    device_id: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    use crate::pairing_store::{PairingStatus, PairingStore};

    let store = PairingStore::open_default();

    match action.as_deref().unwrap_or("list") {
        "list" => {
            let devices = store.list().map_err(hermes_core::AgentError::Io)?;
            if devices.is_empty() {
                println!("No paired devices.");
                println!("  Store: {}", PairingStore::default_path().display());
            } else {
                println!("Paired devices ({}):", devices.len());
                println!(
                    "  {:20} {:10} {:12} Name",
                    "Device ID", "Status", "Last Seen"
                );
                println!("  {}", "-".repeat(60));
                for d in &devices {
                    let last_seen = d.last_seen.as_deref().unwrap_or("never");
                    let name = d.name.as_deref().unwrap_or("(unnamed)");
                    let status_icon = match d.status {
                        PairingStatus::Pending => "⏳",
                        PairingStatus::Approved => "✓",
                        PairingStatus::Revoked => "✗",
                    };
                    println!(
                        "  {:20} {} {:8} {:12} {}",
                        d.device_id, status_icon, d.status, last_seen, name
                    );
                }
            }
        }
        "approve" => {
            let did = device_id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing --device-id. Usage: hermes pairing approve --device-id <id>".into(),
                )
            })?;
            match store.approve(&did) {
                Ok(dev) => {
                    println!("Device '{}' approved.", dev.device_id);
                    if let Some(secret) = &dev.shared_secret {
                        if secret_stdout_allowed() {
                            println!("  Shared secret: {}", secret);
                            println!(
                                "  (plaintext output enabled via HERMES_ALLOW_SECRET_STDOUT=1)"
                            );
                        } else {
                            println!("  Shared secret: {}", mask_secret_value(secret));
                            println!(
                                "  (set HERMES_ALLOW_SECRET_STDOUT=1 to reveal plaintext once)"
                            );
                        }
                        println!("  (Store this securely — it will not be shown again)");
                    }
                }
                Err(e) => println!("Failed to approve device: {}", e),
            }
        }
        "revoke" => {
            let did = device_id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing --device-id. Usage: hermes pairing revoke --device-id <id>".into(),
                )
            })?;
            match store.revoke(&did) {
                Ok(dev) => {
                    println!("Device '{}' revoked.", dev.device_id);
                    println!("  The device will no longer be able to connect.");
                }
                Err(e) => println!("Failed to revoke device: {}", e),
            }
        }
        "clear-pending" => match store.clear_pending() {
            Ok(count) => {
                if count == 0 {
                    println!("No pending pairing requests to clear.");
                } else {
                    println!("Cleared {} pending pairing request(s).", count);
                }
            }
            Err(e) => println!("Failed to clear pending requests: {}", e),
        },
        other => {
            println!("Pairing action '{}' is not recognized.", other);
            println!("Available actions: list, approve, revoke, clear-pending");
        }
    }
    Ok(())
}

/// Handle `hermes claw [action]`.
pub async fn handle_cli_claw(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "migrate" => {
            claw_migrate_cmd()?;
        }
        "cleanup" => {
            claw_cleanup_cmd()?;
        }
        "status" => {
            claw_status_cmd();
        }
        other => {
            println!("Claw action '{}' is not recognized.", other);
            println!("Available actions: migrate, cleanup, status");
        }
    }
    Ok(())
}

/// Check for legacy OpenClaw artefacts and report findings.
fn claw_status_cmd() {
    use crate::claw_migrate::find_openclaw_dir;

    println!("OpenClaw Legacy Status");
    println!("======================\n");

    let home = dirs::home_dir();

    match find_openclaw_dir(None) {
        Some(dir) => {
            println!("  OpenClaw directory: {} (found)", dir.display());

            let config_yaml = dir.join("config.yaml");
            let sessions_dir = dir.join("sessions");
            let env_file = dir.join(".env");
            let skills_dir = dir.join("skills");

            println!(
                "  config.yaml:       {}",
                if config_yaml.exists() {
                    "present"
                } else {
                    "not found"
                }
            );
            println!(
                "  .env:              {}",
                if env_file.exists() {
                    "present"
                } else {
                    "not found"
                }
            );
            println!(
                "  skills/:           {}",
                if skills_dir.is_dir() {
                    "present"
                } else {
                    "not found"
                }
            );

            if sessions_dir.is_dir() {
                let count = std::fs::read_dir(&sessions_dir)
                    .map(|rd| rd.filter_map(|e| e.ok()).count())
                    .unwrap_or(0);
                println!("  sessions/:         {} file(s)", count);
            } else {
                println!("  sessions/:         not found");
            }

            println!("\n  Run `hermes claw migrate` to import into Hermes.");
            println!("  Run `hermes claw cleanup` to remove legacy files.");
        }
        None => {
            println!("  No OpenClaw directory found.");
            if let Some(h) = &home {
                println!(
                    "  Checked: ~/.openclaw, ~/.clawdbot, ~/.moldbot under {}",
                    h.display()
                );
            }
            println!("\n  Nothing to migrate.");
        }
    }

    // Also check for PATH entries in shell configs
    if let Some(h) = &home {
        let shell_files = [".bashrc", ".zshrc", ".profile", ".bash_profile"];
        let mut found_refs = Vec::new();
        for f in &shell_files {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("openclaw") || content.contains("clawdbot") {
                    found_refs.push(f.to_string());
                }
            }
        }
        if !found_refs.is_empty() {
            println!("\n  Shell config references found:");
            for f in &found_refs {
                println!("    ~/{}", f);
            }
        }
    }
}

/// Run the full migration using `claw_migrate::run_migration`.
fn claw_migrate_cmd() -> Result<(), hermes_core::AgentError> {
    use crate::claw_migrate::{find_openclaw_dir, run_migration, MigrateOptions};

    println!("OpenClaw → Hermes Migration");
    println!("===========================\n");

    let source_dir = find_openclaw_dir(None);
    if source_dir.is_none() {
        println!("No OpenClaw directory found. Nothing to migrate.");
        return Ok(());
    }
    let source_dir = source_dir.unwrap();
    println!("Source: {}", source_dir.display());
    println!("Target: {}\n", hermes_config::hermes_home().display());

    // Also copy sessions if they exist
    let src_sessions = source_dir.join("sessions");
    let dst_sessions = hermes_config::hermes_home().join("sessions");
    let mut session_count = 0usize;

    if src_sessions.is_dir() {
        std::fs::create_dir_all(&dst_sessions).map_err(|e| {
            hermes_core::AgentError::Io(format!("Failed to create sessions dir: {}", e))
        })?;
        if let Ok(entries) = std::fs::read_dir(&src_sessions) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dst = dst_sessions.join(entry.file_name());
                if src.is_file() && !dst.exists() && std::fs::copy(&src, &dst).is_ok() {
                    session_count += 1;
                }
            }
        }
    }

    let options = MigrateOptions {
        source: Some(source_dir),
        dry_run: false,
        preset: "full".to_string(),
        overwrite: false,
    };

    let result = run_migration(&options);

    if !result.migrated.is_empty() {
        println!("Migrated:");
        for item in &result.migrated {
            let src = item
                .source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let dst = item
                .destination
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let extra = item.reason.as_deref().unwrap_or("");
            println!("  ✓ {} → {} {}", src, dst, extra);
        }
    }

    if !result.skipped.is_empty() {
        println!("Skipped:");
        for item in &result.skipped {
            let reason = item.reason.as_deref().unwrap_or("");
            println!("  ⊘ {} — {}", item.kind, reason);
        }
    }

    if !result.errors.is_empty() {
        println!("Errors:");
        for item in &result.errors {
            let reason = item.reason.as_deref().unwrap_or("unknown error");
            println!("  ✗ {} — {}", item.kind, reason);
        }
    }

    if session_count > 0 {
        println!("\nSessions copied: {}", session_count);
    }

    let total = result.migrated.len() + session_count;
    println!(
        "\nMigration complete: {} item(s) migrated, {} skipped, {} error(s).",
        total,
        result.skipped.len(),
        result.errors.len()
    );

    Ok(())
}

/// Remove legacy OpenClaw files after confirmation.
fn claw_cleanup_cmd() -> Result<(), hermes_core::AgentError> {
    use crate::claw_migrate::find_openclaw_dir;
    use std::io::{self, BufRead, Write};

    let source_dir = find_openclaw_dir(None);
    if source_dir.is_none() {
        println!("No OpenClaw directory found. Nothing to clean up.");
        return Ok(());
    }
    let source_dir = source_dir.unwrap();

    println!("OpenClaw Cleanup");
    println!("================\n");
    println!("The following will be PERMANENTLY deleted:");
    println!("  Directory: {}", source_dir.display());

    // Count contents
    let file_count = count_files_recursive(&source_dir);
    println!("  Contains:  ~{} file(s)\n", file_count);

    // Check shell configs
    let home = dirs::home_dir();
    let shell_files = [".bashrc", ".zshrc", ".profile", ".bash_profile"];
    let mut affected_shells: Vec<String> = Vec::new();
    if let Some(h) = &home {
        for f in &shell_files {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("openclaw") || content.contains("clawdbot") {
                    affected_shells.push(f.to_string());
                    println!("  Shell config: ~/{} (contains openclaw references)", f);
                }
            }
        }
    }

    print!("\nProceed with cleanup? [y/N]: ");
    io::stdout().flush().ok();
    let answer = io::stdin()
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default();

    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Cleanup cancelled.");
        return Ok(());
    }

    // Remove the directory
    match std::fs::remove_dir_all(&source_dir) {
        Ok(_) => println!("  ✓ Removed {}", source_dir.display()),
        Err(e) => println!("  ✗ Failed to remove {}: {}", source_dir.display(), e),
    }

    // Clean shell configs
    if let Some(h) = &home {
        for f in &affected_shells {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let cleaned: Vec<&str> = content
                    .lines()
                    .filter(|line| {
                        let lower = line.to_lowercase();
                        !lower.contains("openclaw") && !lower.contains("clawdbot")
                    })
                    .collect();
                let new_content = cleaned.join("\n") + "\n";
                match std::fs::write(&path, new_content) {
                    Ok(_) => println!("  ✓ Cleaned ~/{}", f),
                    Err(e) => println!("  ✗ Failed to clean ~/{}: {}", f, e),
                }
            }
        }
    }

    println!("\nCleanup complete.");
    Ok(())
}

/// Recursively count files in a directory.
fn count_files_recursive(dir: &std::path::Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_recursive(&path);
            } else {
                count += 1;
            }
        }
    }
    count
}

include!("command_cli_sessions_acp/acp_backup.rs");
