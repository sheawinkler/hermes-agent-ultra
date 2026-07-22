//! Shared platform helper functions.
//!
//! Common text manipulation utilities used across platform adapters.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use hermes_core::{Message, MessageRole};

/// Image reference extracted from generated markdown / HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImageRef {
    pub url: String,
    pub alt_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaMarkerRef {
    pub path: String,
    pub as_voice: bool,
}

/// Split a long message into chunks that respect word and sentence boundaries.
///
/// Prefers breaking at sentence endings (`. `, `! `, `? `), then at newlines,
/// then at word boundaries (spaces), and only hard-splits as a last resort.
pub fn split_long_message(text: &str, max_len: usize) -> Vec<String> {
    if max_len == 0 {
        return vec![text.to_string()];
    }
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        let window = &remaining[..max_len];

        // Try sentence boundary first
        let break_at = find_last_sentence_break(window)
            .or_else(|| window.rfind('\n'))
            .or_else(|| window.rfind(' '))
            .unwrap_or(max_len);

        let break_at = if break_at == 0 { max_len } else { break_at };

        chunks.push(remaining[..break_at].trim_end().to_string());
        remaining = remaining[break_at..].trim_start();
    }

    chunks
}

fn find_last_sentence_break(text: &str) -> Option<usize> {
    let terminators = [". ", "! ", "? ", ".\n", "!\n", "?\n"];
    terminators
        .iter()
        .filter_map(|t| text.rfind(t).map(|i| i + t.len()))
        .max()
}

/// Escape Markdown special characters.
pub fn escape_markdown(text: &str) -> String {
    const SPECIAL_CHARS: &[char] = &[
        '\\', '`', '*', '_', '{', '}', '[', ']', '(', ')', '#', '+', '-', '.', '!', '|', '~', '>',
    ];

    let mut result = String::with_capacity(text.len() + text.len() / 8);
    for ch in text.chars() {
        if SPECIAL_CHARS.contains(&ch) {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

/// Truncate text to `max_len` characters, appending an ellipsis if truncated.
pub fn truncate_with_ellipsis(text: &str, max_len: usize) -> String {
    if max_len < 4 {
        return text.chars().take(max_len).collect();
    }
    if text.len() <= max_len {
        return text.to_string();
    }

    let truncated = &text[..text.floor_char_boundary(max_len - 3)];
    // Try to break at a word boundary
    let break_at = truncated.rfind(' ').unwrap_or(truncated.len());
    format!("{}...", &truncated[..break_at])
}

/// Extract all URLs from text.
pub fn extract_urls(text: &str) -> Vec<String> {
    let re = Regex::new(r"https?://[^\s<>\[\](){}]+").expect("valid regex");
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

/// Download a remote media URL into memory while enforcing the gateway media cap.
///
/// Some platform APIs require uploading downloaded image bytes through their
/// native file endpoint. Keep those paths bounded even when the response omits
/// or lies about `Content-Length`.
pub async fn download_media_url(
    client: &reqwest::Client,
    url: &str,
) -> Result<(Vec<u8>, Option<String>), String> {
    download_media_url_with_limit(client, url, crate::media::DEFAULT_INBOUND_MEDIA_MAX_BYTES).await
}

pub async fn download_media_url_with_limit(
    client: &reqwest::Client,
    url: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, Option<String>), String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }

    if max_bytes > 0 {
        if let Some(content_length) = resp.content_length() {
            if content_length > max_bytes {
                return Err(format!(
                    "Content-Length {content_length} exceeds media cap {max_bytes}"
                ));
            }
        }
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let mut resp = resp;
    let mut bytes = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("read body failed: {e}"))?
    {
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| "media body size overflow".to_string())?;
        if max_bytes > 0 && next_len as u64 > max_bytes {
            return Err(format!(
                "downloaded body {next_len} exceeds media cap {max_bytes}"
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    if bytes.is_empty() {
        return Err("empty body".to_string());
    }
    Ok((bytes, content_type))
}

/// Extract inline images from markdown and HTML.
///
/// Supports:
/// - Markdown images: `![alt](https://...)`
/// - HTML images: `<img src="https://...">`
///
/// Returns `(cleaned_text, images)` where matched image tags are removed from
/// the text and captured in `images`.
pub fn extract_inline_images(text: &str) -> (String, Vec<InlineImageRef>) {
    let md_re = Regex::new(r"!\[([^\]]*)\]\((https?://[^\s)]+)\)").expect("valid regex");
    let html_re =
        Regex::new(r#"<img[^>]*\bsrc=["']?(https?://[^"' >]+)["']?[^>]*>"#).expect("valid regex");
    let code_ranges = markdown_code_ranges(text);

    let mut matches = Vec::new();
    for captures in md_re.captures_iter(text) {
        let Some(full) = captures.get(0) else {
            continue;
        };
        if overlaps_markdown_code(full.start(), full.end(), &code_ranges) {
            continue;
        }
        let url = captures
            .get(2)
            .map(|value| value.as_str().trim())
            .unwrap_or_default();
        let alt_text = captures
            .get(1)
            .map(|value| value.as_str().trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        matches.push((
            full.start(),
            full.end(),
            InlineImageRef {
                url: url.to_string(),
                alt_text,
            },
        ));
    }
    for captures in html_re.captures_iter(text) {
        let Some(full) = captures.get(0) else {
            continue;
        };
        if overlaps_markdown_code(full.start(), full.end(), &code_ranges) {
            continue;
        }
        let url = captures
            .get(1)
            .map(|value| value.as_str().trim())
            .unwrap_or_default();
        matches.push((
            full.start(),
            full.end(),
            InlineImageRef {
                url: url.to_string(),
                alt_text: None,
            },
        ));
    }
    matches.sort_by_key(|(start, _, _)| *start);

    let mut cleaned = String::with_capacity(text.len());
    let mut images = Vec::with_capacity(matches.len());
    let mut cursor = 0;
    let mut normalize_seam = false;
    for (start, end, image) in matches {
        if start < cursor {
            continue;
        }
        append_inline_image_segment(&mut cleaned, &text[cursor..start], normalize_seam);
        images.push(image);
        cursor = end;
        normalize_seam = true;
    }
    append_inline_image_segment(&mut cleaned, &text[cursor..], normalize_seam);
    if normalize_seam && cursor == text.len() {
        while matches!(cleaned.as_bytes().last(), Some(b' ' | b'\t')) {
            cleaned.pop();
        }
    }
    if cleaned.trim().is_empty() {
        cleaned.clear();
    }

    (cleaned, images)
}

fn markdown_code_ranges(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut fenced = Vec::new();
    let mut line_start = 0;

    while line_start < bytes.len() {
        let (line_end, next_line) = markdown_line_bounds(bytes, line_start);
        let opener = markdown_fence_marker(bytes, line_start, line_end).filter(
            |(marker, _, marker_end)| {
                *marker != b'`' || !bytes[*marker_end..line_end].contains(&b'`')
            },
        );
        let Some((marker, minimum_run, _)) = opener else {
            line_start = next_line;
            continue;
        };

        let mut candidate_start = next_line;
        let mut fence_end = bytes.len();
        while candidate_start < bytes.len() {
            let (candidate_end, following_line) =
                markdown_line_bounds(bytes, candidate_start);
            if let Some((candidate_marker, run, marker_end)) =
                markdown_fence_marker(bytes, candidate_start, candidate_end)
            {
                let suffix_is_blank = bytes[marker_end..candidate_end]
                    .iter()
                    .all(|byte| matches!(byte, b' ' | b'\t'));
                if candidate_marker == marker && run >= minimum_run && suffix_is_blank {
                    fence_end = following_line;
                    break;
                }
            }
            candidate_start = following_line;
        }
        fenced.push((line_start, fence_end));
        line_start = fence_end;
    }

    let mut ranges = fenced.clone();
    let mut cursor = 0;
    for (fence_start, fence_end) in fenced {
        append_inline_code_ranges(bytes, cursor, fence_start, &mut ranges);
        cursor = fence_end;
    }
    append_inline_code_ranges(bytes, cursor, bytes.len(), &mut ranges);
    ranges.sort_unstable_by_key(|(start, _)| *start);
    ranges
}

fn markdown_line_bounds(bytes: &[u8], start: usize) -> (usize, usize) {
    let newline = bytes[start..].iter().position(|byte| *byte == b'\n');
    match newline {
        Some(relative) => {
            let raw_end = start + relative;
            let line_end = raw_end
                .checked_sub(1)
                .filter(|index| bytes[*index] == b'\r')
                .unwrap_or(raw_end);
            (line_end, raw_end + 1)
        }
        None => (bytes.len(), bytes.len()),
    }
}

fn markdown_fence_marker(
    bytes: &[u8],
    line_start: usize,
    line_end: usize,
) -> Option<(u8, usize, usize)> {
    let mut marker_start = line_start;
    while marker_start < line_end
        && marker_start - line_start < 3
        && bytes[marker_start] == b' '
    {
        marker_start += 1;
    }
    let marker = *bytes.get(marker_start)?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let marker_end = bytes[marker_start..line_end]
        .iter()
        .take_while(|byte| **byte == marker)
        .count()
        + marker_start;
    let run = marker_end - marker_start;
    (run >= 3).then_some((marker, run, marker_end))
}

fn append_inline_code_ranges(
    bytes: &[u8],
    start: usize,
    end: usize,
    ranges: &mut Vec<(usize, usize)>,
) {
    let mut paragraph_start = start;
    let mut line_start = start;
    while line_start < end {
        let (line_end, next_line) = markdown_line_bounds(bytes, line_start);
        let next_line = next_line.min(end);
        let is_blank = bytes[line_start..line_end.min(end)]
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t'));
        if is_blank {
            append_inline_code_range_segment(bytes, paragraph_start, line_start, ranges);
            paragraph_start = next_line;
        }
        line_start = next_line;
    }
    append_inline_code_range_segment(bytes, paragraph_start, end, ranges);
}

fn append_inline_code_range_segment(
    bytes: &[u8],
    start: usize,
    end: usize,
    ranges: &mut Vec<(usize, usize)>,
) {
    let mut runs = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let Some(relative) = bytes[cursor..end]
            .iter()
            .position(|byte| *byte == b'`')
        else {
            break;
        };
        let run_start = cursor + relative;
        let run_end = bytes[run_start..end]
            .iter()
            .take_while(|byte| **byte == b'`')
            .count()
            + run_start;
        runs.push((run_start, run_end, run_end - run_start));
        cursor = run_end;
    }

    let mut next_same = vec![None; runs.len()];
    let mut last_by_length = HashMap::new();
    for (index, (_, _, length)) in runs.iter().enumerate().rev() {
        next_same[index] = last_by_length.insert(*length, index);
    }
    let mut index = 0;
    while index < runs.len() {
        if let Some(close_index) = next_same[index] {
            ranges.push((runs[index].0, runs[close_index].1));
            index = close_index + 1;
        } else {
            index += 1;
        }
    }
}

fn overlaps_markdown_code(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(code_start, code_end)| start < *code_end && end > *code_start)
}

fn markdown_code_contains(index: usize, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(code_start, code_end)| index >= *code_start && index < *code_end)
}

fn append_inline_image_segment(output: &mut String, segment: &str, normalize_seam: bool) {
    if !normalize_seam {
        output.push_str(segment);
        return;
    }
    let should_trim_horizontal = output.is_empty()
        || matches!(output.as_bytes().last(), Some(b' ' | b'\t'));
    if should_trim_horizontal {
        let leading_horizontal = segment
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == b' ' || **byte == b'\t')
            .count();
        output.push_str(&segment[leading_horizontal..]);
    } else {
        output.push_str(segment);
    }
}

fn clean_media_marker_path(raw: &str) -> String {
    let mut cleaned = raw
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`'))
        .to_string();
    while cleaned
        .chars()
        .last()
        .is_some_and(|c| matches!(c, ',' | '}' | ']' | '"' | '\'' | '`'))
    {
        cleaned.pop();
    }
    cleaned.trim().to_string()
}

fn has_active_voice_directive(text: &str) -> bool {
    let voice_re = Regex::new(r"\[\[audio_as_voice\]\]").expect("valid regex");
    let code_ranges = markdown_code_ranges(text);
    let active = voice_re
        .find_iter(text)
        .any(|marker| !overlaps_markdown_code(marker.start(), marker.end(), &code_ranges));
    active
}

fn terminal_unclosed_fence_closer(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut line_start = 0;
    while line_start < bytes.len() {
        let (line_end, next_line) = markdown_line_bounds(bytes, line_start);
        let opener = markdown_fence_marker(bytes, line_start, line_end).filter(
            |(marker, _, marker_end)| {
                *marker != b'`' || !bytes[*marker_end..line_end].contains(&b'`')
            },
        );
        let Some((marker, minimum_run, _)) = opener else {
            line_start = next_line;
            continue;
        };

        let mut candidate_start = next_line;
        let mut closed_at = None;
        while candidate_start < bytes.len() {
            let (candidate_end, following_line) =
                markdown_line_bounds(bytes, candidate_start);
            if let Some((candidate_marker, run, marker_end)) =
                markdown_fence_marker(bytes, candidate_start, candidate_end)
            {
                let suffix_is_blank = bytes[marker_end..candidate_end]
                    .iter()
                    .all(|byte| matches!(byte, b' ' | b'\t'));
                if candidate_marker == marker && run >= minimum_run && suffix_is_blank {
                    closed_at = Some(following_line);
                    break;
                }
            }
            candidate_start = following_line;
        }
        match closed_at {
            Some(next) => line_start = next,
            None => return Some((marker as char).to_string().repeat(minimum_run)),
        }
    }
    None
}

/// Extract `MEDIA:<path>` output markers and `[[audio_as_voice]]` directives.
///
/// Tool backends such as browser screenshots and TTS can return media paths
/// inside tool JSON or plain text. The gateway strips these markers from the
/// chat text and delivers the local paths through platform-native file APIs.
pub fn extract_media_markers(text: &str) -> (String, Vec<MediaMarkerRef>) {
    let voice_re = Regex::new(r"\[\[audio_as_voice\]\]").expect("valid regex");
    let media_re = Regex::new(r"MEDIA:([^\r\n]+)").expect("valid regex");
    let code_ranges = markdown_code_ranges(text);

    let mut removals = Vec::new();
    let mut as_voice = false;
    for marker in voice_re.find_iter(text) {
        if overlaps_markdown_code(marker.start(), marker.end(), &code_ranges) {
            continue;
        }
        as_voice = true;
        removals.push((marker.start(), marker.end()));
    }

    let mut media = Vec::new();
    for captures in media_re.captures_iter(text) {
        let Some(full) = captures.get(0) else {
            continue;
        };
        if markdown_code_contains(full.start(), &code_ranges) {
            continue;
        }
        let path = captures
            .get(1)
            .map(|value| clean_media_marker_path(value.as_str()))
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        media.push(MediaMarkerRef { path, as_voice });
        removals.push((full.start(), full.end()));
    }

    if removals.is_empty() {
        return (text.to_string(), media);
    }
    removals.sort_unstable_by_key(|(start, _)| *start);

    let mut cleaned = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut normalize_seam = false;
    for (start, end) in removals {
        if start < cursor {
            continue;
        }
        append_media_marker_segment(&mut cleaned, &text[cursor..start], normalize_seam);
        cursor = end;
        normalize_seam = true;
    }
    append_media_marker_segment(&mut cleaned, &text[cursor..], normalize_seam);
    if cleaned.trim().is_empty() {
        cleaned.clear();
    }

    (cleaned, media)
}

/// Build the user-visible reply for the latest agent turn.
///
/// Only tool results after the latest user message may contribute media. This
/// prevents TTS artifacts in conversation history from being redelivered on
/// every later turn. `media_tag` is parsed as a JSON field rather than scanning
/// arbitrary tool output for path-like text.
pub fn extract_current_turn_gateway_reply(messages: &[Message]) -> String {
    let turn_start = messages
        .iter()
        .rposition(|message| message.role == MessageRole::User)
        .map(|index| index + 1)
        .unwrap_or(0);
    let current_turn = &messages[turn_start..];
    let mut reply = current_turn
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::Assistant)
        .and_then(|message| message.content.clone())
        .unwrap_or_default();

    let (_, existing_media) = extract_media_markers(&reply);
    let reply_has_voice_directive = has_active_voice_directive(&reply);
    let mut seen_paths: HashSet<String> = existing_media
        .into_iter()
        .map(|media| media.path)
        .collect();
    let mut tool_requested_voice = false;
    let mut new_paths = Vec::new();

    for message in current_turn
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
    {
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
            continue;
        };
        let Some(media_tag) = value.get("media_tag").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let (_, media) = extract_media_markers(media_tag);
        tool_requested_voice |= media.iter().any(|item| item.as_voice);
        for item in media {
            if seen_paths.insert(item.path.clone()) {
                new_paths.push(item.path);
            }
        }
    }

    let needs_voice_directive = tool_requested_voice && !reply_has_voice_directive;
    if needs_voice_directive || !new_paths.is_empty() {
        if let Some(closer) = terminal_unclosed_fence_closer(&reply) {
            append_gateway_reply_line(&mut reply, &closer);
        }
    }
    if needs_voice_directive {
        append_gateway_reply_line(&mut reply, "[[audio_as_voice]]");
    }
    for path in new_paths {
        append_gateway_reply_line(&mut reply, &format!("MEDIA:{path}"));
    }

    if reply.trim().is_empty() {
        "(no assistant reply)".to_string()
    } else {
        reply
    }
}

fn append_gateway_reply_line(reply: &mut String, line: &str) {
    if !reply.is_empty() && !reply.ends_with('\n') {
        reply.push('\n');
    }
    reply.push_str(line);
}

fn append_media_marker_segment(output: &mut String, segment: &str, normalize_seam: bool) {
    if !normalize_seam {
        output.push_str(segment);
        return;
    }

    let leading_horizontal = if output.is_empty()
        || matches!(output.as_bytes().last(), Some(b' ' | b'\t'))
    {
        segment
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == b' ' || **byte == b'\t')
            .count()
    } else {
        0
    };
    let segment = &segment[leading_horizontal..];
    let trailing_breaks = trailing_line_break_count(output);
    let leading_breaks = leading_line_break_count(segment);
    let retained_leading_breaks = 2usize.saturating_sub(trailing_breaks);
    let skipped_breaks = leading_breaks.saturating_sub(retained_leading_breaks);
    let skipped_bytes = byte_offset_after_line_breaks(segment, skipped_breaks);
    output.push_str(&segment[skipped_bytes..]);
}

fn trailing_line_break_count(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut cursor = bytes.len();
    let mut count = 0;
    while cursor > 0 && bytes[cursor - 1] == b'\n' {
        count += 1;
        cursor -= 1;
        if cursor > 0 && bytes[cursor - 1] == b'\r' {
            cursor -= 1;
        }
    }
    count
}

fn leading_line_break_count(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut count = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\n' {
            cursor += 1;
            count += 1;
        } else if bytes[cursor] == b'\r'
            && bytes.get(cursor + 1).is_some_and(|byte| *byte == b'\n')
        {
            cursor += 2;
            count += 1;
        } else {
            break;
        }
    }
    count
}

fn byte_offset_after_line_breaks(value: &str, count: usize) -> usize {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut consumed = 0;
    while cursor < bytes.len() && consumed < count {
        if bytes[cursor] == b'\n' {
            cursor += 1;
        } else if bytes[cursor] == b'\r'
            && bytes.get(cursor + 1).is_some_and(|byte| *byte == b'\n')
        {
            cursor += 2;
        } else {
            break;
        }
        consumed += 1;
    }
    cursor
}

/// Format a code block with optional language tag.
pub fn format_code_block(code: &str, lang: Option<&str>) -> String {
    match lang {
        Some(l) if !l.is_empty() => format!("```{}\n{}\n```", l, code),
        _ => format!("```\n{}\n```", code),
    }
}

/// Sanitize HTML by stripping tags, keeping only text content.
pub fn sanitize_html(text: &str) -> String {
    let re = Regex::new(r"<[^>]+>").expect("valid regex");
    let cleaned = re.replace_all(text, "");
    // Decode common HTML entities
    cleaned
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Estimate reading time in seconds for the given text.
///
/// Assumes an average reading speed of 200 words per minute.
pub fn estimate_read_time(text: &str) -> u32 {
    let word_count = text.split_whitespace().count() as f64;
    let minutes = word_count / 200.0;
    (minutes * 60.0).ceil() as u32
}

/// Detect MIME type from a file extension.
pub fn mime_from_extension(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "pdf" => "application/pdf",
        "doc" | "docx" => "application/msword",
        "xls" | "xlsx" => "application/vnd.ms-excel",
        "zip" => "application/zip",
        "json" => "application/json",
        "xml" => "application/xml",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

/// Determine the file's media category from its extension.
pub fn media_category(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "tiff" => "image",
        "mp4" | "webm" | "mov" | "avi" | "mkv" => "video",
        "mp3" | "ogg" | "oga" | "wav" | "flac" | "aac" | "m4a" => "audio",
        _ => "document",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct StaticTtsBackend {
        result: String,
    }

    #[async_trait::async_trait]
    impl hermes_tools::tools::tts::TtsBackend for StaticTtsBackend {
        async fn synthesize(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _provider: Option<&str>,
            _output_path: Option<&str>,
            _speed: Option<f64>,
        ) -> Result<String, hermes_core::ToolError> {
            Ok(self.result.clone())
        }
    }

    #[test]
    fn test_split_long_message_short() {
        let chunks = split_long_message("hello world", 100);
        assert_eq!(chunks, vec!["hello world"]);
    }

    #[test]
    fn test_split_long_message_sentence_break() {
        let text = "First sentence. Second sentence. Third sentence is long.";
        let chunks = split_long_message(text, 35);
        assert!(chunks.len() >= 2);
        assert!(chunks[0].ends_with('.'));
    }

    #[test]
    fn test_escape_markdown() {
        assert_eq!(escape_markdown("hello *world*"), "hello \\*world\\*");
        assert_eq!(escape_markdown("no_special"), "no\\_special");
    }

    #[test]
    fn test_truncate_with_ellipsis() {
        assert_eq!(truncate_with_ellipsis("short", 100), "short");
        let result = truncate_with_ellipsis("this is a long sentence that should be truncated", 20);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 20);
    }

    #[test]
    fn test_extract_urls() {
        let text = "Visit https://example.com and http://foo.bar/baz for more.";
        let urls = extract_urls(text);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].starts_with("https://"));
    }


    #[test]
    fn test_extract_inline_images_markdown_and_html() {
        let text = "Start ![chart](https://cdn.example.com/a.png) and <img src=\"https://assets.example/render?id=1\"> end";
        let (cleaned, images) = extract_inline_images(text);
        assert_eq!(cleaned, "Start and end");
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].url, "https://cdn.example.com/a.png");
        assert_eq!(images[0].alt_text.as_deref(), Some("chart"));
        assert_eq!(images[1].url, "https://assets.example/render?id=1");
        assert_eq!(images[1].alt_text, None);
    }

    #[test]
    fn inline_images_preserve_multiline_markdown_and_source_order() {
        let text = "# Report\n\n- first <img src=\"https://cdn.example.com/first.png\"> item\n- second\n\n```rust\nlet value = 42;\n```\n\nTail ![chart](https://cdn.example.com/second.webp) text";
        let (cleaned, images) = extract_inline_images(text);

        assert_eq!(
            cleaned,
            "# Report\n\n- first item\n- second\n\n```rust\nlet value = 42;\n```\n\nTail text"
        );
        assert_eq!(
            images
                .iter()
                .map(|image| image.url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "https://cdn.example.com/first.png",
                "https://cdn.example.com/second.webp"
            ]
        );
        assert_eq!(images[1].alt_text.as_deref(), Some("chart"));
    }

    #[test]
    fn inline_images_ignore_markdown_and_html_inside_code() {
        let text = "Inline `![sample](https://cdn.example.com/inline.png)` and `<img src=\"https://cdn.example.com/inline-html.png\">`.\n\n```md\n![sample](https://cdn.example.com/fenced.png)\n<img src=\"https://cdn.example.com/fenced-html.png\">\n```\n\nLive ![chart](https://cdn.example.com/live.png)";
        let (cleaned, images) = extract_inline_images(text);

        assert_eq!(
            cleaned,
            "Inline `![sample](https://cdn.example.com/inline.png)` and `<img src=\"https://cdn.example.com/inline-html.png\">`.\n\n```md\n![sample](https://cdn.example.com/fenced.png)\n<img src=\"https://cdn.example.com/fenced-html.png\">\n```\n\nLive"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "https://cdn.example.com/live.png");
    }

    #[test]
    fn unmatched_backtick_does_not_hide_images_after_paragraph_boundary() {
        let text = "An unmatched ` delimiter\n\nLive ![chart](https://cdn.example.com/live.png)";
        let (cleaned, images) = extract_inline_images(text);

        assert_eq!(cleaned, "An unmatched ` delimiter\n\nLive");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].url, "https://cdn.example.com/live.png");
    }

    #[test]
    fn inline_image_seams_trim_horizontal_boundaries_but_preserve_newlines() {
        let (terminal, terminal_images) =
            extract_inline_images("Text ![one](https://cdn.example.com/one.png)");
        assert_eq!(terminal, "Text");
        assert_eq!(terminal_images.len(), 1);

        let (consecutive, consecutive_images) = extract_inline_images(
            "![one](https://cdn.example.com/one.png)  ![two](https://cdn.example.com/two.png)",
        );
        assert_eq!(consecutive, "");
        assert_eq!(consecutive_images.len(), 2);

        let (multiline, _) =
            extract_inline_images("A\n![one](https://cdn.example.com/one.png)\nB");
        assert_eq!(multiline, "A\n\nB");
    }

    #[test]
    fn test_extract_inline_images_keeps_ordinary_html_links() {
        let text = "A <a href=\"https://example.com/not-image\">ordinary link</a> B";
        let (cleaned, images) = extract_inline_images(text);
        assert_eq!(images.len(), 0);
        assert_eq!(cleaned, text);
    }

    #[tokio::test]
    async fn download_media_url_with_limit_preserves_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![1, 2, 3, 4]),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let (bytes, content_type) = download_media_url_with_limit(&client, &server.uri(), 8)
            .await
            .expect("download media");
        assert_eq!(bytes, vec![1, 2, 3, 4]);
        assert_eq!(content_type.as_deref(), Some("image/png"));
    }

    #[tokio::test]
    async fn download_media_url_with_limit_rejects_oversized_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(vec![1, 2, 3, 4, 5]),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = download_media_url_with_limit(&client, &server.uri(), 4)
            .await
            .expect_err("oversized body should fail");
        assert!(
            err.contains("Content-Length") || err.contains("downloaded body"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_extract_media_markers_strips_tags_and_preserves_voice_directive() {
        let text =
            "Here\n[[audio_as_voice]]\nMEDIA:/tmp/audio.ogg\nMEDIA:\"/tmp/screenshot.png\",\nDone";
        let (cleaned, media) = extract_media_markers(text);
        assert_eq!(cleaned, "Here\n\nDone");
        assert_eq!(
            media,
            vec![
                MediaMarkerRef {
                    path: "/tmp/audio.ogg".to_string(),
                    as_voice: true,
                },
                MediaMarkerRef {
                    path: "/tmp/screenshot.png".to_string(),
                    as_voice: true,
                },
            ]
        );
    }

    #[test]
    fn media_marker_parser_preserves_unrelated_text_verbatim() {
        let text = "  Alpha\n\n\nBeta  ";
        let (cleaned, media) = extract_media_markers(text);

        assert_eq!(cleaned, text);
        assert!(media.is_empty());
    }

    #[test]
    fn media_markers_ignore_code_but_accept_backtick_quoted_paths() {
        let text = "Before\nMEDIA:`/tmp/live image.png`\nAfter\n\nInline `MEDIA:/tmp/code.png`\n\n```text\nMEDIA:/tmp/fenced.png\n[[audio_as_voice]]\n```";
        let (cleaned, media) = extract_media_markers(text);

        assert_eq!(
            cleaned,
            "Before\n\nAfter\n\nInline `MEDIA:/tmp/code.png`\n\n```text\nMEDIA:/tmp/fenced.png\n[[audio_as_voice]]\n```"
        );
        assert_eq!(
            media,
            vec![MediaMarkerRef {
                path: "/tmp/live image.png".to_string(),
                as_voice: false,
            }]
        );
    }

    #[test]
    fn current_turn_gateway_reply_ignores_historical_media() {
        let messages = vec![
            Message::user("old audio"),
            Message::tool_result(
                "old-tool",
                r#"{"media_tag":"[[audio_as_voice]]\nMEDIA:/tmp/old.ogg"}"#,
            ),
            Message::assistant("Old reply"),
            Message::user("new question"),
            Message::assistant("Current reply"),
        ];

        assert_eq!(
            extract_current_turn_gateway_reply(&messages),
            "Current reply"
        );
    }

    #[test]
    fn current_turn_gateway_reply_appends_structured_tool_media() {
        let messages = vec![
            Message::user("say this"),
            Message::assistant_with_tool_calls(None, Vec::new()),
            Message::tool_result(
                "tts-tool",
                r#"{"provider":"copy","media_tag":"[[audio_as_voice]]\nMEDIA:/tmp/current.ogg"}"#,
            ),
            Message::assistant("Audio is ready."),
        ];

        assert_eq!(
            extract_current_turn_gateway_reply(&messages),
            "Audio is ready.\n[[audio_as_voice]]\nMEDIA:/tmp/current.ogg"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_tts_result_flows_through_current_turn_gateway_extraction() {
        use hermes_core::ToolHandler;
        use hermes_tools::backends::tts::MultiTtsBackend;
        use hermes_tools::tools::tts::TextToSpeechHandler;

        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("gateway-voice.ogg");
        let backend = MultiTtsBackend::with_tts_config(serde_json::json!({
            "provider": "copy-tts",
            "providers": {
                "copy-tts": {
                    "type": "command",
                    "command": "cp {input_path} {output_path}",
                    "output_format": "ogg",
                    "voice_compatible": true
                }
            }
        }));
        let handler = TextToSpeechHandler::new(std::sync::Arc::new(backend));
        let tool_result = handler
            .execute(serde_json::json!({
                "text": "gateway audio",
                "provider": "copy-tts",
                "output_path": output
            }))
            .await
            .unwrap();
        let messages = vec![
            Message::user("speak"),
            Message::assistant_with_tool_calls(None, Vec::new()),
            Message::tool_result("tts-tool", tool_result),
            Message::assistant("Audio is ready."),
        ];

        let reply = extract_current_turn_gateway_reply(&messages);
        let (cleaned, media) = extract_media_markers(&reply);
        assert_eq!(cleaned.trim(), "Audio is ready.");
        assert_eq!(
            media,
            vec![MediaMarkerRef {
                path: output.display().to_string(),
                as_voice: true,
            }]
        );
        assert_eq!(std::fs::read_to_string(output).unwrap(), "gateway audio");
    }

    #[tokio::test]
    async fn builtin_tts_result_is_normalized_before_gateway_extraction() {
        use hermes_core::ToolHandler;
        use hermes_tools::tools::tts::TextToSpeechHandler;

        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("builtin.mp3");
        std::fs::write(&output, b"audio").unwrap();
        let backend = StaticTtsBackend {
            result: serde_json::json!({
                "provider": "openai",
                "transport": "direct",
                "file": output
            })
            .to_string(),
        };
        let handler = TextToSpeechHandler::new(std::sync::Arc::new(backend));
        let tool_result = handler
            .execute(serde_json::json!({"text": "speak"}))
            .await
            .unwrap();
        let messages = vec![
            Message::user("speak"),
            Message::tool_result("tts-tool", tool_result),
            Message::assistant("Audio is ready."),
        ];

        let reply = extract_current_turn_gateway_reply(&messages);
        let (_, media) = extract_media_markers(&reply);
        assert_eq!(
            media,
            vec![MediaMarkerRef {
                path: output.display().to_string(),
                as_voice: false,
            }]
        );
    }

    #[test]
    fn current_turn_gateway_reply_deduplicates_model_repeated_media() {
        let messages = vec![
            Message::user("two clips"),
            Message::tool_result(
                "tts-tool",
                r#"{"media_tag":"[[audio_as_voice]]\nMEDIA:/tmp/same.ogg\nMEDIA:/tmp/new.ogg"}"#,
            ),
            Message::assistant("Ready\nMEDIA:/tmp/same.ogg"),
        ];

        let reply = extract_current_turn_gateway_reply(&messages);
        assert_eq!(reply.matches("MEDIA:/tmp/same.ogg").count(), 1);
        assert_eq!(reply.matches("MEDIA:/tmp/new.ogg").count(), 1);
        assert_eq!(reply.matches("[[audio_as_voice]]").count(), 1);
    }

    #[test]
    fn current_turn_gateway_reply_does_not_scan_arbitrary_tool_text() {
        let messages = vec![
            Message::user("ordinary tool"),
            Message::tool_result(
                "other-tool",
                r#"{"note":"MEDIA:/tmp/not-an-artifact.ogg"}"#,
            ),
            Message::assistant("No media."),
        ];

        assert_eq!(
            extract_current_turn_gateway_reply(&messages),
            "No media."
        );
    }

    #[test]
    fn current_turn_gateway_reply_ignores_voice_directive_inside_code() {
        let messages = vec![
            Message::user("show and speak"),
            Message::tool_result(
                "tts-tool",
                r#"{"media_tag":"[[audio_as_voice]]\nMEDIA:/tmp/live.ogg"}"#,
            ),
            Message::assistant("Example:\n```text\n[[audio_as_voice]]\n```\nReady"),
        ];

        let reply = extract_current_turn_gateway_reply(&messages);
        assert_eq!(reply.matches("[[audio_as_voice]]").count(), 2);
        let (_, media) = extract_media_markers(&reply);
        assert_eq!(
            media,
            vec![MediaMarkerRef {
                path: "/tmp/live.ogg".to_string(),
                as_voice: true,
            }]
        );
    }

    #[test]
    fn current_turn_gateway_reply_reuses_active_standalone_voice_directive() {
        let messages = vec![
            Message::user("speak"),
            Message::tool_result(
                "tts-tool",
                r#"{"media_tag":"[[audio_as_voice]]\nMEDIA:/tmp/live.ogg"}"#,
            ),
            Message::assistant("Ready\n[[audio_as_voice]]"),
        ];

        let reply = extract_current_turn_gateway_reply(&messages);
        assert_eq!(reply.matches("[[audio_as_voice]]").count(), 1);
        let (_, media) = extract_media_markers(&reply);
        assert_eq!(media.len(), 1);
        assert!(media[0].as_voice);
    }

    #[test]
    fn current_turn_gateway_reply_closes_truncated_fence_before_media_suffix() {
        let messages = vec![
            Message::user("speak"),
            Message::tool_result(
                "tts-tool",
                r#"{"media_tag":"[[audio_as_voice]]\nMEDIA:/tmp/live.ogg"}"#,
            ),
            Message::assistant("Ready\n```text\npartial"),
        ];

        let reply = extract_current_turn_gateway_reply(&messages);
        assert!(reply.contains("partial\n```\n[[audio_as_voice]]"));
        let (_, media) = extract_media_markers(&reply);
        assert_eq!(
            media,
            vec![MediaMarkerRef {
                path: "/tmp/live.ogg".to_string(),
                as_voice: true,
            }]
        );
    }

    #[test]
    fn current_turn_gateway_reply_never_replays_prior_assistant_content() {
        let messages = vec![
            Message::user("old"),
            Message::assistant("stale reply"),
            Message::user("latest"),
            Message::assistant_with_tool_calls(None, Vec::new()),
        ];

        assert_eq!(
            extract_current_turn_gateway_reply(&messages),
            "(no assistant reply)"
        );
    }

    #[test]
    fn test_format_code_block() {
        assert_eq!(
            format_code_block("let x = 1;", Some("rust")),
            "```rust\nlet x = 1;\n```"
        );
        assert_eq!(format_code_block("hello", None), "```\nhello\n```");
    }

    #[test]
    fn test_sanitize_html() {
        assert_eq!(
            sanitize_html("<b>bold</b> &amp; <i>italic</i>"),
            "bold & italic"
        );
    }

    #[test]
    fn test_estimate_read_time() {
        let words_200: String = (0..200).map(|_| "word").collect::<Vec<_>>().join(" ");
        let time = estimate_read_time(&words_200);
        assert_eq!(time, 60);
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension("png"), "image/png");
        assert_eq!(mime_from_extension("mp4"), "video/mp4");
        assert_eq!(mime_from_extension("xyz"), "application/octet-stream");
    }

    #[test]
    fn test_media_category() {
        assert_eq!(media_category("jpg"), "image");
        assert_eq!(media_category("mp4"), "video");
        assert_eq!(media_category("mp3"), "audio");
        assert_eq!(media_category("pdf"), "document");
    }
}
