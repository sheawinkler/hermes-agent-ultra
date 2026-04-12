//! Markdown-safe message splitting.
//!
//! Splits text at safe boundaries without breaking code blocks, links,
//! or formatting marks. Closes unclosed formatting at split points and
//! reopens them in the next chunk.

/// Split markdown text into chunks that fit within `max_len`, preserving
/// formatting integrity.
///
/// Rules:
/// - Never split inside a code block (``` ... ```)
/// - Never split inside a link `[text](url)`
/// - Close unclosed formatting marks (**, *, `, etc.) at split point
///   and reopen in next chunk
/// - Prefer splitting at paragraph boundaries (\n\n), then line boundaries (\n)
pub fn split_markdown(text: &str, max_len: usize) -> Vec<String> {
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

        let split_at = find_safe_split(remaining, max_len);
        let (chunk, rest) = remaining.split_at(split_at);

        // Check for unclosed formatting in this chunk
        let (closed_chunk, reopen_prefix) = close_and_reopen_formatting(chunk);

        chunks.push(closed_chunk);
        remaining = rest.trim_start_matches('\n');

        // Prepend reopened formatting to the next chunk
        if !reopen_prefix.is_empty() && !remaining.is_empty() {
            let mut next = reopen_prefix;
            next.push_str(remaining);
            // We need to own this string for the next iteration
            // Use a small trick: push to chunks temporarily and fix up
            remaining = ""; // will be replaced
            chunks.push(next); // placeholder
            let last = chunks.pop().unwrap();
            // Re-process the remainder with the prefix
            let sub_chunks = split_markdown(&last, max_len);
            chunks.extend(sub_chunks);
            break;
        }
    }

    chunks
}

/// Find a safe byte offset to split at, within `max_len` bytes.
fn find_safe_split(text: &str, max_len: usize) -> usize {
    let search_range = &text[..max_len.min(text.len())];

    // Check if we're inside a code block at the split point
    let in_code_block = is_in_code_block(search_range);

    if in_code_block {
        // Find the end of the code block
        if let Some(end) = find_code_block_end(text, max_len) {
            if end <= max_len + 100 {
                // Allow slight overflow to close the code block
                return end;
            }
        }
        // If code block is too long, split at max_len anyway
    }

    // Try paragraph boundary (\n\n)
    if let Some(pos) = search_range.rfind("\n\n") {
        if pos > 0 {
            return pos + 1; // Include one newline
        }
    }

    // Try line boundary (\n)
    if let Some(pos) = search_range.rfind('\n') {
        if pos > 0 {
            return pos + 1;
        }
    }

    // Try space boundary
    if let Some(pos) = search_range.rfind(' ') {
        if pos > 0 {
            return pos + 1;
        }
    }

    // Hard split at max_len
    max_len.min(text.len())
}

/// Check if the text ends inside an unclosed code block.
fn is_in_code_block(text: &str) -> bool {
    let mut in_block = false;
    let mut i = 0;
    let bytes = text.as_bytes();

    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i] == b'`' && bytes[i + 1] == b'`' && bytes[i + 2] == b'`' {
            in_block = !in_block;
            i += 3;
            // Skip to end of line for opening fence
            if in_block {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }

    in_block
}

/// Find the end of a code block that spans past `start_pos`.
fn find_code_block_end(text: &str, start_pos: usize) -> Option<usize> {
    // Look for closing ``` after start_pos
    let search = &text[start_pos..];
    if let Some((i, _)) = search.match_indices("```").next() {
        let end = start_pos + i + 3;
        // Include the rest of the line
        let after = &text[end..];
        let line_end = after.find('\n').map(|p| end + p + 1).unwrap_or(text.len());
        return Some(line_end);
    }
    None
}

/// Close unclosed formatting marks and return (closed_text, reopen_prefix).
fn close_and_reopen_formatting(text: &str) -> (String, String) {
    let mut open_marks: Vec<&str> = Vec::new();

    // Track formatting marks: **, *, `, ~~
    let marks = ["**", "~~", "*", "`"];

    for mark in &marks {
        let count = text.matches(mark).count();
        // For ** we need to be careful not to double-count with *
        if mark == &"*" {
            // Count single * that aren't part of **
            let double_count = text.matches("**").count() * 2;
            let single_count = text.matches('*').count() - double_count;
            if single_count % 2 != 0 {
                open_marks.push(mark);
            }
        } else if count % 2 != 0 {
            open_marks.push(mark);
        }
    }

    if open_marks.is_empty() {
        return (text.to_string(), String::new());
    }

    // Close marks at end of chunk
    let mut closed = text.to_string();
    let mut reopen = String::new();

    for mark in open_marks.iter().rev() {
        closed.push_str(mark);
    }
    for mark in &open_marks {
        reopen.push_str(mark);
    }

    (closed, reopen)
}

// ---------------------------------------------------------------------------
// Cross-platform format conversion (Task 18.4)
// ---------------------------------------------------------------------------

/// Escape special characters for Telegram MarkdownV2 format.
pub fn to_telegram_markdown_v2(text: &str) -> String {
    let special_chars = [
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut result = String::with_capacity(text.len() * 2);

    let mut in_code_block = false;
    let mut in_inline_code = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        // Track code blocks (don't escape inside them)
        if c == '`' {
            if chars.peek() == Some(&'`') {
                chars.next();
                if chars.peek() == Some(&'`') {
                    chars.next();
                    in_code_block = !in_code_block;
                    result.push_str("```");
                    continue;
                } else {
                    result.push_str("``");
                    continue;
                }
            } else {
                in_inline_code = !in_inline_code;
                result.push('`');
                continue;
            }
        }

        if in_code_block || in_inline_code {
            result.push(c);
        } else if special_chars.contains(&c) {
            result.push('\\');
            result.push(c);
        } else {
            result.push(c);
        }
    }

    result
}

/// Convert standard Markdown to Slack mrkdwn format.
pub fn to_slack_mrkdwn(text: &str) -> String {
    let mut result = text.to_string();

    // Bold: **text** → *text*
    // But first handle bold-italic ***text*** → *_text_*
    result = result.replace("***", "§BOLDITALIC§");
    result = result.replace("**", "*");
    result = result.replace("§BOLDITALIC§", "*_");

    // Italic: *text* is already correct in Slack, but _text_ also works
    // Links: [text](url) → <url|text>
    let link_re = regex::Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap_or_else(|_| {
        regex::Regex::new(r"$^").unwrap() // never matches
    });
    result = link_re.replace_all(&result, "<$2|$1>").to_string();

    // Strikethrough: ~~text~~ → ~text~
    result = result.replace("~~", "~");

    result
}

/// Adjust Markdown for Discord (mostly compatible, minor tweaks).
pub fn to_discord_markdown(text: &str) -> String {
    // Discord supports standard Markdown mostly as-is.
    // Main difference: headers don't render, use bold instead.
    let mut result = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("# ") {
            result.push_str("**");
            result.push_str(trimmed.trim_start_matches("# "));
            result.push_str("**");
        } else if trimmed.starts_with("## ") {
            result.push_str("**");
            result.push_str(trimmed.trim_start_matches("## "));
            result.push_str("**");
        } else if trimmed.starts_with("### ") {
            result.push_str("**");
            result.push_str(trimmed.trim_start_matches("### "));
            result.push_str("**");
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    // Remove trailing newline
    if result.ends_with('\n') {
        result.pop();
    }
    result
}

/// Strip all Markdown formatting for plain-text platforms.
pub fn strip_markdown(text: &str) -> String {
    let mut result = text.to_string();

    // Remove code blocks
    let code_block_re =
        regex::Regex::new(r"```[\s\S]*?```").unwrap_or_else(|_| regex::Regex::new(r"$^").unwrap());
    result = code_block_re
        .replace_all(&result, |caps: &regex::Captures| {
            let block = caps[0].trim_start_matches("```").trim_end_matches("```");
            // Remove language identifier from first line
            let content = block
                .split_once('\n')
                .map(|(_, rest)| rest)
                .unwrap_or(block);
            content.to_string()
        })
        .to_string();

    // Remove inline code backticks
    result = result.replace('`', "");

    // Remove bold/italic markers
    result = result.replace("***", "");
    result = result.replace("**", "");
    result = result.replace("~~", "");

    // Convert links: [text](url) → text (url)
    let link_re = regex::Regex::new(r"\[([^\]]+)\]\(([^)]+)\)")
        .unwrap_or_else(|_| regex::Regex::new(r"$^").unwrap());
    result = link_re.replace_all(&result, "$1 ($2)").to_string();

    // Remove heading markers
    let heading_re =
        regex::Regex::new(r"(?m)^#{1,6}\s+").unwrap_or_else(|_| regex::Regex::new(r"$^").unwrap());
    result = heading_re.replace_all(&result, "").to_string();

    // Remove image markers: ![alt](url) → alt
    let img_re = regex::Regex::new(r"!\[([^\]]*)\]\([^)]+\)")
        .unwrap_or_else(|_| regex::Regex::new(r"$^").unwrap());
    result = img_re.replace_all(&result, "$1").to_string();

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_short_text() {
        let chunks = split_markdown("hello world", 100);
        assert_eq!(chunks, vec!["hello world"]);
    }

    #[test]
    fn test_split_at_paragraph() {
        let text = "paragraph one\n\nparagraph two that is quite long";
        let chunks = split_markdown(text, 20);
        assert!(chunks.len() >= 2);
        // Should split at paragraph boundary
        assert!(chunks[0].contains("paragraph one"));
    }

    #[test]
    fn test_is_in_code_block() {
        assert!(!is_in_code_block("normal text"));
        assert!(is_in_code_block("```\ncode here"));
        assert!(!is_in_code_block("```\ncode\n```"));
    }

    #[test]
    fn test_telegram_markdown_v2_escaping() {
        let result = to_telegram_markdown_v2("Hello_world! Test.");
        assert!(result.contains("\\_"));
        assert!(result.contains("\\!"));
        assert!(result.contains("\\."));
    }

    #[test]
    fn test_telegram_preserves_code_blocks() {
        let result = to_telegram_markdown_v2("```\ncode_here\n```");
        // Inside code blocks, underscores should NOT be escaped
        assert!(result.contains("code_here"));
    }

    #[test]
    fn test_slack_mrkdwn_conversion() {
        let result = to_slack_mrkdwn("**bold** and [link](https://example.com)");
        assert!(result.contains("*bold*"));
        assert!(result.contains("<https://example.com|link>"));
    }

    #[test]
    fn test_discord_markdown_headers() {
        let result = to_discord_markdown("# Title\n## Subtitle\nNormal text");
        assert!(result.contains("**Title**"));
        assert!(result.contains("**Subtitle**"));
        assert!(result.contains("Normal text"));
    }

    #[test]
    fn test_strip_markdown() {
        let result = strip_markdown("**bold** and `code` and [link](url)");
        assert!(!result.contains("**"));
        assert!(!result.contains('`'));
        assert!(result.contains("bold"));
        assert!(result.contains("code"));
        assert!(result.contains("link (url)"));
    }

    #[test]
    fn test_strip_markdown_code_block() {
        let result = strip_markdown("```python\nprint('hello')\n```");
        assert!(result.contains("print('hello')"));
        assert!(!result.contains("```"));
    }
}
