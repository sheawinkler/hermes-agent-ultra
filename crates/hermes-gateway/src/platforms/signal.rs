//! Signal messaging adapter via signal-cli REST API.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::platforms::signal_rate_limit;

// ---------------------------------------------------------------------------
// Incoming message types
// ---------------------------------------------------------------------------

/// Parsed incoming Signal message from the signal-cli receive endpoint.
#[derive(Debug, Clone)]
pub struct IncomingSignalMessage {
    pub source: String,
    pub timestamp: u64,
    pub text: String,
    pub group_id: Option<String>,
    pub attachments: Vec<String>,
}

// ---------------------------------------------------------------------------
// SignalConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalConfig {
    /// Signal number (e.g., "+1234567890").
    pub phone_number: String,
    /// Signal CLI REST API URL.
    #[serde(default = "default_signal_api_url")]
    pub api_url: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_signal_api_url() -> String {
    "http://localhost:8080".to_string()
}

// ---------------------------------------------------------------------------
// SignalAdapter
// ---------------------------------------------------------------------------

pub struct SignalAdapter {
    base: BasePlatformAdapter,
    config: SignalConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl SignalAdapter {
    pub fn new(config: SignalConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.phone_number).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
        })
    }

    pub fn config(&self) -> &SignalConfig {
        &self.config
    }

    /// Send a message via signal-cli REST API.
    pub async fn send_text(&self, recipient: &str, text: &str) -> Result<(), GatewayError> {
        let url = format!("{}/v2/send", self.config.api_url);
        let (plain_text, styles) = markdown_to_signal(text);
        let mut body = serde_json::json!({
            "message": plain_text,
            "number": self.config.phone_number,
            "recipients": [recipient]
        });
        apply_signal_styles_to_body(&mut body, &styles);

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Signal send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Signal API error: {}",
                text
            )));
        }
        Ok(())
    }

    /// Parse a single message from signal-cli's receive endpoint into a typed struct.
    ///
    /// Expects the signal-cli JSON envelope format with `envelope.dataMessage`.
    pub fn parse_received_message(msg: &serde_json::Value) -> Option<IncomingSignalMessage> {
        let envelope = msg.get("envelope")?;
        let source = envelope.get("source").and_then(|v| v.as_str())?.to_string();
        let timestamp = envelope
            .get("timestamp")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let data_message = envelope.get("dataMessage")?;
        let text = data_message
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let group_id = data_message
            .get("groupInfo")
            .and_then(|g| g.get("groupId"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let attachments = data_message
            .get("attachments")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Some(IncomingSignalMessage {
            source,
            timestamp,
            text,
            group_id,
            attachments,
        })
    }

    /// Receive messages via signal-cli REST API polling.
    pub async fn receive_messages(&self) -> Result<Vec<serde_json::Value>, GatewayError> {
        let url = format!(
            "{}/v1/receive/{}",
            self.config.api_url, self.config.phone_number
        );
        let resp =
            self.client.get(&url).send().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Signal receive failed: {}", e))
            })?;

        let messages: Vec<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Signal parse failed: {}", e)))?;
        Ok(messages)
    }
}

fn apply_signal_styles_to_body(body: &mut serde_json::Value, styles: &[String]) {
    if styles.is_empty() {
        return;
    }
    if styles.len() == 1 {
        body["textStyle"] = serde_json::Value::String(styles[0].clone());
    } else {
        body["textStyles"] = serde_json::Value::Array(
            styles
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        );
    }
}

fn collapse_excess_newlines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut newline_count = 0usize;
    for ch in input.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                out.push(ch);
            }
        } else {
            newline_count = 0;
            out.push(ch);
        }
    }
    out.trim().to_string()
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn append_styled(out: &mut String, styles: &mut Vec<String>, text: &str, style: &str) {
    if text.is_empty() {
        return;
    }
    let start = utf16_len(out);
    let len = utf16_len(text);
    out.push_str(text);
    styles.push(format!("{start}:{len}:{style}"));
}

fn is_line_start(input: &str, byte_idx: usize) -> bool {
    byte_idx == 0 || input[..byte_idx].ends_with('\n')
}

fn find_same_line_marker(haystack: &str, marker: &str) -> Option<usize> {
    let marker_pos = haystack.find(marker)?;
    let newline_pos = haystack.find('\n');
    if newline_pos.is_some_and(|pos| pos < marker_pos) {
        None
    } else {
        Some(marker_pos)
    }
}

fn prev_char(input: &str, byte_idx: usize) -> Option<char> {
    input[..byte_idx].chars().next_back()
}

fn next_char(input: &str, byte_idx: usize) -> Option<char> {
    input[byte_idx..].chars().next()
}

fn is_ascii_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn single_asterisk_close_offset(rest: &str) -> Option<usize> {
    let mut search_from = 1usize;
    while search_from < rest.len() {
        let found = find_same_line_marker(&rest[search_from..], "*")?;
        let close = search_from + found;
        let before = prev_char(rest, close);
        let after = next_char(rest, close + 1);
        if before != Some('*') && after != Some('*') {
            return Some(close);
        }
        search_from = close + 1;
    }
    None
}

fn single_underscore_close_offset(rest: &str) -> Option<usize> {
    let mut search_from = 1usize;
    while search_from < rest.len() {
        let found = find_same_line_marker(&rest[search_from..], "_")?;
        let close = search_from + found;
        let before = prev_char(rest, close);
        let after = next_char(rest, close + 1);
        if before != Some('_') && after.is_none_or(|ch| !is_ascii_word(ch)) {
            return Some(close);
        }
        search_from = close + 1;
    }
    None
}

fn append_next_char(input: &str, byte_idx: &mut usize, out: &mut String) {
    let ch = input[*byte_idx..]
        .chars()
        .next()
        .expect("byte index must point at a char boundary");
    out.push(ch);
    *byte_idx += ch.len_utf8();
}

/// Convert Markdown-like Signal output into signal-cli plain text plus
/// UTF-16 body range descriptors (`start:length:STYLE`).
pub fn markdown_to_signal(text: &str) -> (String, Vec<String>) {
    let input = collapse_excess_newlines(text);
    if input.is_empty() {
        return (String::new(), Vec::new());
    }

    let mut out = String::with_capacity(input.len());
    let mut styles = Vec::new();
    let mut i = 0usize;
    while i < input.len() {
        let rest = &input[i..];

        if let Some(after_fence) = rest.strip_prefix("```") {
            if let Some(close_rel) = after_fence.find("```") {
                let block = &after_fence[..close_rel];
                let code = if let Some(newline) = block.find('\n') {
                    &block[newline + 1..]
                } else {
                    block
                };
                append_styled(
                    &mut out,
                    &mut styles,
                    code.trim_end_matches('\n'),
                    "MONOSPACE",
                );
                i += 3 + close_rel + 3;
                continue;
            }
        }

        if is_line_start(&input, i) && rest.starts_with('#') {
            let hashes = rest.chars().take_while(|ch| *ch == '#').count();
            if (1..=6).contains(&hashes) {
                let marker_len = hashes;
                if next_char(rest, marker_len).is_some_and(char::is_whitespace) {
                    let after_marker = rest[marker_len..].trim_start_matches([' ', '\t']);
                    let consumed_ws = rest[marker_len..].len() - after_marker.len();
                    let content_start = marker_len + consumed_ws;
                    let line_len = rest[content_start..]
                        .find('\n')
                        .unwrap_or(rest.len() - content_start);
                    let line = &rest[content_start..content_start + line_len];
                    append_styled(&mut out, &mut styles, line, "BOLD");
                    i += content_start + line_len;
                    continue;
                }
            }
        }

        if let Some(close) = rest
            .strip_prefix("**")
            .and_then(|tail| find_same_line_marker(tail, "**"))
            .map(|pos| pos + 2)
        {
            let inner = &rest[2..close];
            append_styled(&mut out, &mut styles, inner, "BOLD");
            i += close + 2;
            continue;
        }

        if let Some(close) = rest
            .strip_prefix("__")
            .and_then(|tail| find_same_line_marker(tail, "__"))
            .map(|pos| pos + 2)
        {
            let inner = &rest[2..close];
            append_styled(&mut out, &mut styles, inner, "BOLD");
            i += close + 2;
            continue;
        }

        if let Some(close) = rest
            .strip_prefix("~~")
            .and_then(|tail| find_same_line_marker(tail, "~~"))
            .map(|pos| pos + 2)
        {
            let inner = &rest[2..close];
            append_styled(&mut out, &mut styles, inner, "STRIKETHROUGH");
            i += close + 2;
            continue;
        }

        if let Some(close) = rest
            .strip_prefix('`')
            .and_then(|tail| find_same_line_marker(tail, "`"))
            .map(|pos| pos + 1)
        {
            let inner = &rest[1..close];
            append_styled(&mut out, &mut styles, inner, "MONOSPACE");
            i += close + 1;
            continue;
        }

        if rest.starts_with('*')
            && !rest.starts_with("**")
            && !rest.starts_with("* ")
            && !next_char(rest, 1).is_some_and(char::is_whitespace)
        {
            if let Some(close) = single_asterisk_close_offset(rest) {
                let inner = &rest[1..close];
                append_styled(&mut out, &mut styles, inner, "ITALIC");
                i += close + 1;
                continue;
            }
        }

        if rest.starts_with('_')
            && !rest.starts_with("__")
            && !next_char(rest, 1).is_some_and(char::is_whitespace)
            && prev_char(&input, i).is_none_or(|ch| !is_ascii_word(ch))
        {
            if let Some(close) = single_underscore_close_offset(rest) {
                let inner = &rest[1..close];
                append_styled(&mut out, &mut styles, inner, "ITALIC");
                i += close + 1;
                continue;
            }
        }

        append_next_char(&input, &mut i, &mut out);
    }

    (out, styles)
}

fn normalized_image_content_type(content_type: Option<&str>) -> Option<String> {
    let normalized = content_type?
        .split(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_ascii_lowercase();
    if normalized.starts_with("image/") {
        Some(normalized)
    } else {
        None
    }
}

fn image_extension_from_content_type(content_type: Option<&str>) -> Option<&'static str> {
    let normalized = normalized_image_content_type(content_type)?;
    match normalized.as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/svg+xml" => Some("svg"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/avif" => Some("avif"),
        _ => None,
    }
}

fn remote_image_file_name(image_url: &str, content_type: Option<&str>) -> String {
    let stripped = image_url
        .split('#')
        .next()
        .unwrap_or(image_url)
        .split('?')
        .next()
        .unwrap_or(image_url)
        .trim_end_matches('/');
    let base = stripped.rsplit('/').next().unwrap_or("").trim();
    let mut file_name = if base.is_empty() {
        "image".to_string()
    } else {
        base.to_string()
    };

    let has_extension = std::path::Path::new(&file_name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some();
    if !has_extension {
        let ext = image_extension_from_content_type(content_type).unwrap_or("png");
        file_name.push('.');
        file_name.push_str(ext);
    }
    file_name
}

fn image_fallback_text(image_url: &str, caption: Option<&str>) -> String {
    match caption.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => format!("{c}\n{image_url}"),
        None => image_url.to_string(),
    }
}

#[async_trait]
impl PlatformAdapter for SignalAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Signal adapter starting (number: {})",
            self.config.phone_number
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Signal adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("Signal does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let b64 = base64_encode(&file_bytes);

        let scheduler = signal_rate_limit::get_scheduler();
        let started = Instant::now();
        let waited = {
            let mut scheduler = scheduler.lock().await;
            scheduler.acquire(1).await.map_err(|err| {
                GatewayError::SendFailed(format!("Signal attachment scheduler error: {err}"))
            })?
        };
        if waited >= signal_rate_limit::SIGNAL_BATCH_PACING_NOTICE_THRESHOLD {
            debug!(
                waited = %signal_rate_limit::format_wait(waited),
                "Signal attachment send paced by scheduler"
            );
        }

        let url = format!("{}/v2/send", self.config.api_url);
        let body = serde_json::json!({
            "message": caption.unwrap_or(""),
            "number": self.config.phone_number,
            "recipients": [chat_id],
            "base64_attachments": [b64]
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Signal attachment send failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            if signal_rate_limit::is_signal_rate_limit_error(&text) {
                let retry_after = signal_rate_limit::extract_retry_after_seconds(&text);
                scheduler.lock().await.feedback(retry_after, 1);
            }
            return Err(GatewayError::SendFailed(format!(
                "Signal attachment error: {text}"
            )));
        }
        scheduler
            .lock()
            .await
            .report_rpc_duration(started.elapsed(), 1);
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        if let Some(path) = image_url.strip_prefix("file://") {
            let decoded_path = urlencoding::decode(path)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| path.to_string());
            return self.send_file(chat_id, &decoded_path, caption).await;
        }

        let downloaded = async {
            let resp = self
                .client
                .get(image_url)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("status {}", resp.status()));
            }

            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("read body failed: {e}"))?
                .to_vec();
            if bytes.is_empty() {
                return Err("empty body".to_string());
            }
            Ok((bytes, content_type))
        }
        .await;

        let (bytes, content_type) = match downloaded {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Signal image-url download failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                return self
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await;
            }
        };

        let file_name = remote_image_file_name(image_url, content_type.as_deref());
        let suffix = std::path::Path::new(&file_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_else(|| ".png".to_string());
        let temp_path = std::env::temp_dir().join(format!(
            "hermes_signal_img_{}{}",
            uuid::Uuid::new_v4(),
            suffix
        ));
        tokio::fs::write(&temp_path, &bytes).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to write temp image file: {e}"))
        })?;

        let temp_path_str = temp_path.to_string_lossy().to_string();
        let send_result = self.send_file(chat_id, &temp_path_str, caption).await;
        if let Err(err) = tokio::fs::remove_file(&temp_path).await {
            warn!(
                path = %temp_path.display(),
                error = %err,
                "Failed to remove temporary Signal image file"
            );
        }

        match send_result {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Signal image upload failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                self.send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await
            }
        }
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "signal"
    }
}

/// Simple base64 encoding using the `base64` crate convention (standard alphabet).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn style_types(styles: &[String]) -> Vec<&str> {
        styles
            .iter()
            .filter_map(|style| style.rsplit(':').next())
            .collect()
    }

    fn styles_with_type<'a>(styles: &'a [String], style_type: &str) -> Vec<&'a String> {
        styles
            .iter()
            .filter(|style| style.ends_with(&format!(":{style_type}")))
            .collect()
    }

    fn extract_utf16(text: &str, style: &str) -> String {
        let parts: Vec<_> = style.split(':').collect();
        let start = parts[0].parse::<usize>().expect("style start");
        let len = parts[1].parse::<usize>().expect("style len");
        let encoded: Vec<u16> = text.encode_utf16().collect();
        String::from_utf16(&encoded[start..start + len]).expect("valid utf16 slice")
    }

    #[test]
    fn remote_image_file_name_keeps_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/path/diagram.png?token=abc",
            Some("image/png"),
        );
        assert_eq!(file_name, "diagram.png");
    }

    #[test]
    fn remote_image_file_name_adds_extension_from_content_type() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/jpeg"));
        assert_eq!(file_name, "diagram.jpg");
    }

    #[test]
    fn image_fallback_text_with_caption() {
        let text = image_fallback_text("https://cdn.example.com/path/diagram", Some("Figure 1"));
        assert_eq!(text, "Figure 1\nhttps://cdn.example.com/path/diagram");
    }

    #[test]
    fn signal_markdown_basic_styles_strip_markers() {
        let (text, styles) = markdown_to_signal("**bold** and *italic* and ~~strike~~");
        assert_eq!(text, "bold and italic and strike");
        let mut types = style_types(&styles);
        types.sort_unstable();
        assert_eq!(types, vec!["BOLD", "ITALIC", "STRIKETHROUGH"]);
    }

    #[test]
    fn signal_markdown_handles_inline_and_fenced_code() {
        let (text, styles) = markdown_to_signal("run `ls -la`\n```python\nprint('hello')\n```");
        assert!(text.contains("ls -la"));
        assert!(text.contains("print('hello')"));
        assert!(!text.contains("```"));
        assert!(!text.contains("python"));
        assert_eq!(styles_with_type(&styles, "MONOSPACE").len(), 2);
    }

    #[test]
    fn signal_markdown_headings_become_bold_ranges() {
        let (text, styles) = markdown_to_signal("## First\n\nSome text\n\n### Second");
        assert_eq!(text, "First\n\nSome text\n\nSecond");
        assert_eq!(styles_with_type(&styles, "BOLD").len(), 2);
        assert!(!text.contains("##"));
    }

    #[test]
    fn signal_markdown_avoids_italic_false_positives() {
        for input in [
            "the config_file is ready",
            "set OPENAI_API_KEY and ANTHROPIC_API_KEY",
            "/tools/delegate_tool.py",
            "* item one\n* item two\n* item three",
            "*foo\nbar*",
            "_foo\nbar_",
        ] {
            let (_text, styles) = markdown_to_signal(input);
            assert!(
                styles_with_type(&styles, "ITALIC").is_empty(),
                "unexpected italic style for {input:?}: {styles:?}"
            );
        }
    }

    #[test]
    fn signal_markdown_keeps_italic_inside_bullet() {
        let (text, styles) = markdown_to_signal("* this has *emphasis* inside\n* plain item");
        assert!(text.contains("emphasis"));
        assert_eq!(styles_with_type(&styles, "ITALIC").len(), 1);
    }

    #[test]
    fn signal_markdown_positions_are_utf16_body_ranges() {
        let (text, styles) = markdown_to_signal("🎉🎉 **test**");
        assert_eq!(text, "🎉🎉 test");
        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0], "5:4:BOLD");
        assert_eq!(extract_utf16(&text, &styles[0]), "test");
    }

    #[test]
    fn signal_markdown_collapses_excess_newlines_and_preserves_links() {
        let (text, styles) =
            markdown_to_signal("first\n\n\n\nsecond\nCheck [link](https://example.com)");
        assert!(!text.contains("\n\n\n"));
        assert!(text.contains("https://example.com"));
        assert!(styles.is_empty());
    }

    #[tokio::test]
    async fn send_text_posts_signal_style_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/send"))
            .and(body_partial_json(json!({
                "message": "hello world",
                "number": "+15551234567",
                "recipients": ["+15559876543"],
                "textStyle": "6:5:BOLD"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"timestamp": 1})))
            .mount(&server)
            .await;

        let adapter = SignalAdapter::new(SignalConfig {
            phone_number: "+15551234567".to_string(),
            api_url: server.uri(),
            proxy: AdapterProxyConfig::default(),
        })
        .expect("adapter");

        adapter
            .send_text("+15559876543", "hello **world**")
            .await
            .expect("send text");
    }

    #[tokio::test]
    async fn send_text_posts_multiple_signal_styles() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/send"))
            .and(body_partial_json(json!({
                "message": "bold and italic",
                "textStyles": ["0:4:BOLD", "9:6:ITALIC"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"timestamp": 1})))
            .mount(&server)
            .await;

        let adapter = SignalAdapter::new(SignalConfig {
            phone_number: "+15551234567".to_string(),
            api_url: server.uri(),
            proxy: AdapterProxyConfig::default(),
        })
        .expect("adapter");

        adapter
            .send_text("+15559876543", "**bold** and *italic*")
            .await
            .expect("send text");
    }
}
