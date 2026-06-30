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

/// Split a message into chunks that fit within the given max length.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let end = (start + max_len).min(text.len());

        if end >= text.len() {
            chunks.push(text[start..].to_string());
            break;
        }

        let break_at = text[start..end]
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
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
