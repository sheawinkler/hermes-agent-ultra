// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

fn reactions_toggle_enabled(raw: Option<&str>, default_enabled: bool) -> bool {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(value) => {
            let lowered = value.to_ascii_lowercase();
            !matches!(lowered.as_str(), "false" | "0" | "no")
        }
        None => default_enabled,
    }
}

fn slack_event_is_dm(envelope: &SocketModeEnvelope, channel_id: &str) -> bool {
    let channel_type = envelope
        .payload
        .as_ref()
        .and_then(|payload| payload.get("event"))
        .and_then(|event| event.get("channel_type"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    matches!(channel_type, "im" | "mpim") || channel_id.starts_with('D')
}

fn slack_value_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_slack_scope_header(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn slack_mime_key(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn slack_filename_ext(file_name: &str) -> Option<String> {
    Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
}

fn slack_stt_extension_supported(ext: &str) -> bool {
    SLACK_STT_SUPPORTED_EXTS.contains(&ext)
}

fn resolve_slack_audio_ext(file_name: Option<&str>, mimetype: Option<&str>) -> String {
    if let Some(ext) = file_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(slack_filename_ext)
        .filter(|ext| slack_stt_extension_supported(ext))
    {
        return ext;
    }

    let mime_key = mimetype.map(slack_mime_key).unwrap_or_default();
    if let Some((_, ext)) = SLACK_AUDIO_MIME_TO_EXT
        .iter()
        .find(|(known, _)| *known == mime_key)
    {
        return (*ext).to_string();
    }

    ".m4a".to_string()
}

fn slack_audio_mime_for_ext(ext: &str) -> &'static str {
    SLACK_EXT_TO_AUDIO_MIME
        .iter()
        .find_map(|(known, mime)| (*known == ext).then_some(*mime))
        .unwrap_or("audio/mp4")
}

fn slack_file_is_voice_clip(name: Option<&str>, subtype: Option<&str>) -> bool {
    if subtype
        .map(str::trim)
        .map(|s| s.eq_ignore_ascii_case("slack_audio"))
        .unwrap_or(false)
    {
        return true;
    }

    name.map(str::trim)
        .map(|s| s.to_ascii_lowercase())
        .map(|s| s.starts_with("audio_message"))
        .unwrap_or(false)
}

fn slack_media_kind(
    name: Option<&str>,
    mimetype: Option<&str>,
    subtype: Option<&str>,
) -> SlackMediaKind {
    let mime_key = mimetype.map(slack_mime_key).unwrap_or_default();
    let ext = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(slack_filename_ext);
    let voice_clip = slack_file_is_voice_clip(name, subtype);

    if mime_key.starts_with("audio/") || voice_clip {
        return SlackMediaKind::Audio;
    }

    if matches!(
        ext.as_deref(),
        Some(".m4a" | ".mp3" | ".mpeg" | ".mpga" | ".wav" | ".ogg" | ".aac" | ".flac")
    ) {
        return SlackMediaKind::Audio;
    }

    if mime_key.starts_with("video/")
        || matches!(ext.as_deref(), Some(".mp4" | ".m4v" | ".mov" | ".webm"))
    {
        return SlackMediaKind::Video;
    }

    if mime_key.starts_with("image/")
        || matches!(
            ext.as_deref(),
            Some(".png" | ".jpg" | ".jpeg" | ".gif" | ".webp")
        )
    {
        return SlackMediaKind::Image;
    }

    if mime_key.starts_with("application/")
        || mime_key.starts_with("text/")
        || matches!(
            ext.as_deref(),
            Some(".pdf" | ".md" | ".txt" | ".csv" | ".json" | ".docx" | ".xlsx" | ".pptx" | ".zip")
        )
    {
        return SlackMediaKind::Document;
    }

    SlackMediaKind::Unsupported
}

fn parse_slack_media_files(event: &serde_json::Value) -> Vec<SlackMediaFile> {
    event
        .get("files")
        .and_then(|files| files.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(SlackMediaFile::from_value)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_slack_mention_pattern_values(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        match value {
            serde_json::Value::Array(values) => {
                return values
                    .into_iter()
                    .filter_map(|value| match value {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            serde_json::Value::String(s) => {
                return s
                    .trim()
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }

    trimmed
        .replace('\n', ",")
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn slack_mention_pattern_sources(configured: &[String]) -> Vec<String> {
    let mut patterns: Vec<String> = configured
        .iter()
        .flat_map(|pattern| parse_slack_mention_pattern_values(pattern))
        .collect();
    if patterns.is_empty() {
        if let Ok(raw) = std::env::var("SLACK_MENTION_PATTERNS") {
            patterns = parse_slack_mention_pattern_values(&raw);
        }
    }
    patterns
}

fn compile_slack_mention_patterns(configured: &[String]) -> Vec<Regex> {
    slack_mention_pattern_sources(configured)
        .into_iter()
        .filter_map(
            |pattern| match RegexBuilder::new(&pattern).case_insensitive(true).build() {
                Ok(regex) => Some(regex),
                Err(err) => {
                    warn!(pattern = %pattern, error = %err, "Invalid Slack mention pattern");
                    None
                }
            },
        )
        .collect()
}

fn slack_message_matches_mention_patterns(text: &str, configured: &[String]) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    compile_slack_mention_patterns(configured)
        .iter()
        .any(|pattern| pattern.is_match(text))
}

fn slack_message_is_addressed(
    text: &str,
    bot_user_id: Option<&str>,
    mention_patterns: &[String],
) -> bool {
    if let Some(bot_user_id) = bot_user_id.map(str::trim).filter(|s| !s.is_empty()) {
        if text.contains(&format!("<@{bot_user_id}>")) {
            return true;
        }
    }
    slack_message_matches_mention_patterns(text, mention_patterns)
}

const SLACK_MAX_BLOCKS: usize = 50;
const SLACK_MAX_SECTION_TEXT: usize = 3_000;
const SLACK_MAX_HEADER_TEXT: usize = 150;

fn truncate_slack_text(text: &str, max_chars: usize) -> String {
    let Some((end, _)) = text.char_indices().nth(max_chars) else {
        return text.to_string();
    };
    text[..end].to_string()
}

fn clamp_slack_text_field(
    text_object: &mut serde_json::Map<String, serde_json::Value>,
    max_chars: usize,
) {
    let Some(serde_json::Value::String(text)) = text_object.get_mut("text") else {
        return;
    };
    if text.chars().count() <= max_chars {
        return;
    }

    if max_chars == 0 {
        text.clear();
        return;
    }

    let truncated = truncate_slack_text(text, max_chars.saturating_sub(1));
    *text = format!("{}…", truncated.trim_end());
}

/// Split a message into chunks that fit within Slack's character limit.
///
/// Slack's documented limits are counted in Unicode scalar values, while Rust
/// string indices are bytes. Splitting only at `char_indices` avoids panics and
/// invalid UTF-8 when a multi-byte character crosses a chunk boundary.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if max_len == 0 {
        return Vec::new();
    }
    if text.chars().count() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let remaining = &text[start..];
        let Some((relative_end, _)) = remaining.char_indices().nth(max_len) else {
            chunks.push(remaining.to_string());
            break;
        };
        let end = start + relative_end;
        let break_at = text[start..end]
            .rfind('\n')
            .map(|position| start + position + 1)
            .filter(|position| *position > start)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
}

/// Sanitize a Block Kit payload at the outbound API boundary.
///
/// Slack rejects the entire request when one block is malformed or exceeds a
/// per-block limit. This preserves valid content, removes blocks Slack cannot
/// accept, and returns `None` when callers must use their plain-text fallback.
fn sanitize_slack_blocks(blocks: &serde_json::Value) -> Option<serde_json::Value> {
    let blocks = blocks.as_array()?;
    let mut sanitized = Vec::with_capacity(blocks.len().min(SLACK_MAX_BLOCKS));

    for block in blocks {
        if sanitized.len() == SLACK_MAX_BLOCKS {
            break;
        }

        let serde_json::Value::Object(mut block) = block.clone() else {
            continue;
        };
        let Some(kind) = block.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };

        match kind {
            "section" => {
                let has_body = block
                    .get("fields")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|fields| !fields.is_empty())
                    || block.get("accessory").is_some();
                match block.get_mut("text") {
                    Some(serde_json::Value::Object(text)) => {
                        let has_text = text
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|value| !value.trim().is_empty());
                        if !has_text && !has_body {
                            continue;
                        }
                        clamp_slack_text_field(text, SLACK_MAX_SECTION_TEXT);
                    }
                    _ if !has_body => continue,
                    _ => {}
                }
            }
            "header" => match block.get_mut("text") {
                Some(serde_json::Value::Object(text))
                    if text
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty()) =>
                {
                    clamp_slack_text_field(text, SLACK_MAX_HEADER_TEXT);
                }
                _ => continue,
            },
            "context" => match block.get_mut("elements") {
                Some(serde_json::Value::Array(elements)) if !elements.is_empty() => {
                    for element in elements {
                        if let serde_json::Value::Object(text) = element {
                            clamp_slack_text_field(text, SLACK_MAX_SECTION_TEXT);
                        }
                    }
                }
                _ => continue,
            },
            "rich_text" | "actions" | "context_actions" => {
                if !block
                    .get("elements")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|elements| !elements.is_empty())
                {
                    continue;
                }
            }
            "table" => {
                if !block
                    .get("rows")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|rows| !rows.is_empty())
                {
                    continue;
                }
                if let Some(serde_json::Value::Array(settings)) = block.get("column_settings") {
                    let mut normalized: Vec<_> = settings
                        .iter()
                        .map(|setting| {
                            if setting.is_object() {
                                setting.clone()
                            } else {
                                serde_json::Value::Object(serde_json::Map::new())
                            }
                        })
                        .collect();
                    while normalized
                        .last()
                        .is_some_and(|setting| setting.as_object().is_some_and(|map| map.is_empty()))
                    {
                        normalized.pop();
                    }
                    if normalized.is_empty() {
                        block.remove("column_settings");
                    } else {
                        block.insert(
                            "column_settings".to_string(),
                            serde_json::Value::Array(normalized),
                        );
                    }
                }
            }
            _ => {}
        }

        sanitized.push(serde_json::Value::Object(block));
    }

    (!sanitized.is_empty()).then_some(serde_json::Value::Array(sanitized))
}

fn slack_image_url_blocks(image_url: &str, caption: Option<&str>) -> (serde_json::Value, String) {
    let caption = caption.map(str::trim).filter(|s| !s.is_empty());
    let mut blocks = Vec::new();

    if let Some(text) = caption {
        blocks.push(serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": text }
        }));
    }

    blocks.push(serde_json::json!({
        "type": "image",
        "image_url": image_url,
        "alt_text": caption.unwrap_or("image")
    }));

    let fallback = caption.unwrap_or(image_url).to_string();
    (serde_json::Value::Array(blocks), fallback)
}
