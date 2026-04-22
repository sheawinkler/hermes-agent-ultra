//! Shared platform helper functions.
//!
//! Common text manipulation utilities used across platform adapters.

use regex::Regex;

/// Image reference extracted from generated markdown / HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImageRef {
    pub url: String,
    pub alt_text: Option<String>,
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

/// Extract markdown image links from text.
///
/// Returns `(cleaned_text, image_urls)` where only image tags of the form
/// `![alt](url)` are removed from the cleaned text. Normal markdown links
/// `[text](url)` are preserved.
pub fn extract_markdown_images(text: &str) -> (String, Vec<String>) {
    let image_re = Regex::new(r"!\[[^\]]*\]\(([^)]+)\)").expect("valid regex");
    let mut images = Vec::new();

    let cleaned = image_re
        .replace_all(text, |caps: &regex::Captures| {
            if let Some(url) = caps.get(1).map(|m| m.as_str().trim()) {
                images.push(url.to_string());
            }
            ""
        })
        .to_string();

    let cleaned = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    (cleaned, images)
}

fn looks_like_image_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if lower.contains("fal.media")
        || lower.contains("fal-cdn")
        || lower.contains("replicate.delivery")
    {
        return true;
    }
    let base = lower.split('?').next().unwrap_or(lower.as_str());
    [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg", ".avif", ".heic", ".heif",
        ".tiff", ".tif",
    ]
    .iter()
    .any(|ext| base.ends_with(ext))
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

    let mut images: Vec<InlineImageRef> = Vec::new();

    let without_md = md_re
        .replace_all(text, |caps: &regex::Captures| {
            let url = caps.get(2).map(|m| m.as_str().trim()).unwrap_or_default();
            let alt = caps
                .get(1)
                .map(|m| m.as_str().trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            if looks_like_image_url(url) {
                images.push(InlineImageRef {
                    url: url.to_string(),
                    alt_text: alt,
                });
                "".to_string()
            } else {
                caps.get(0)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default()
            }
        })
        .to_string();

    let without_html = html_re
        .replace_all(&without_md, |caps: &regex::Captures| {
            let url = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
            if looks_like_image_url(url) {
                images.push(InlineImageRef {
                    url: url.to_string(),
                    alt_text: None,
                });
                "".to_string()
            } else {
                caps.get(0)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default()
            }
        })
        .to_string();

    let cleaned = without_html
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    (cleaned, images)
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
    fn test_extract_markdown_images_preserves_non_image_links() {
        let text =
            "See ![diagram](https://img.example.com/a.png) and [doc](https://example.com/a.pdf)";
        let (cleaned, images) = extract_markdown_images(text);
        assert_eq!(images, vec!["https://img.example.com/a.png"]);
        assert_eq!(cleaned, "See and [doc](https://example.com/a.pdf)");
    }

    #[test]
    fn test_extract_markdown_images_multiple_tags() {
        let text = "A ![one](https://i/1.png) B ![two](https://i/2.jpg) C";
        let (cleaned, images) = extract_markdown_images(text);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0], "https://i/1.png");
        assert_eq!(images[1], "https://i/2.jpg");
        assert_eq!(cleaned, "A B C");
    }

    #[test]
    fn test_extract_inline_images_markdown_and_html() {
        let text = "Start ![chart](https://cdn.example.com/a.png) and <img src=\"https://fal.media/b/c\"> end";
        let (cleaned, images) = extract_inline_images(text);
        assert_eq!(cleaned, "Start and end");
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].url, "https://cdn.example.com/a.png");
        assert_eq!(images[0].alt_text.as_deref(), Some("chart"));
        assert_eq!(images[1].url, "https://fal.media/b/c");
        assert_eq!(images[1].alt_text, None);
    }

    #[test]
    fn test_extract_inline_images_keeps_non_image_html() {
        let text = "A <img src=\"https://example.com/not-image\"> B";
        let (cleaned, images) = extract_inline_images(text);
        assert_eq!(images.len(), 0);
        assert_eq!(cleaned, "A <img src=\"https://example.com/not-image\"> B");
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
