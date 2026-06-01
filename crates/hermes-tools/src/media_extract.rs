//! Extract `MEDIA:<path>` tags from message text (parity with Python `BasePlatformAdapter.extract_media`).

use regex::Regex;
use std::sync::LazyLock;

static MEDIA_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        [`"']?
        MEDIA:\s*
        (?P<path>
            `[^`\n]+` |
            "[^"\n]+" |
            '[^'\n]+' |
            (?:[A-Za-z]:[/\\]|~/|/)[^\s`"',;:)}\]]+\.(?:png|jpe?g|gif|webp|mp4|mov|avi|mkv|webm|ogg|opus|mp3|wav|m4a|flac|epub|pdf|zip|rar|7z|docx?|xlsx?|pptx?|txt|md|csv|apk|ipa)
        )
        [`"']?
        "#,
    )
    .expect("valid MEDIA regex")
});

/// Extract media paths and return cleaned message text.
///
/// Returns `(paths, is_voice)` pairs where `is_voice` is true when `[[audio_as_voice]]` appears in the input.
pub fn extract_media(content: &str) -> (Vec<(String, bool)>, String) {
    let has_voice_tag = content.contains("[[audio_as_voice]]");
    let mut cleaned = content
        .replace("[[audio_as_voice]]", "")
        .replace("[[as_document]]", "");

    let mut media = Vec::new();
    for caps in MEDIA_PATTERN.captures_iter(content) {
        let Some(path_match) = caps.name("path") else {
            continue;
        };
        let mut path = path_match.as_str().trim().to_string();
        if path.len() >= 2 {
            let first = path.chars().next().unwrap();
            let last = path.chars().last().unwrap();
            if first == last && matches!(first, '`' | '"' | '\'') {
                path = path[1..path.len() - 1].trim().to_string();
            }
        }
        path = path
            .trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | ',' | '.' | ';' | ':' | ')' | '}' | ']'))
            .to_string();
        if path.is_empty() {
            continue;
        }
        let expanded = hermes_config::resolve_agent_path(&path);
        media.push((expanded.to_string_lossy().into_owned(), has_voice_tag));
    }

    if !media.is_empty() {
        cleaned = MEDIA_PATTERN.replace_all(&cleaned, "").to_string();
        cleaned = Regex::new(r"\n{3,}")
            .expect("valid newline regex")
            .replace_all(&cleaned, "\n\n")
            .trim()
            .to_string();
    } else {
        cleaned = cleaned.trim().to_string();
    }

    (media, cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_media_tag_and_strips_from_message() {
        let (media, cleaned) =
            extract_media("Hello\nMEDIA:/workspace/photo.png\nWorld");
        assert_eq!(media.len(), 1);
        assert!(media[0].0.ends_with("photo.png") || media[0].0.contains("photo.png"));
        assert!(!cleaned.contains("MEDIA:"));
        assert!(cleaned.contains("Hello"));
    }

    #[test]
    fn audio_as_voice_flag_propagates() {
        let (media, _) = extract_media("[[audio_as_voice]]\nMEDIA:~/voice.ogg");
        assert_eq!(media.len(), 1);
        assert!(media[0].1);
    }

    #[test]
    fn extracts_markdown_file_path() {
        let (media, cleaned) = extract_media("Here\nMEDIA:/workspace/AGENTS.md\nDone");
        assert_eq!(media.len(), 1);
        assert!(media[0].0.ends_with("AGENTS.md") || media[0].0.contains("AGENTS.md"));
        assert!(!cleaned.contains("MEDIA:"));
    }

    #[test]
    fn extracts_windows_drive_path() {
        let (media, cleaned) =
            extract_media("MEDIA:C:/code/flowy/hermes-agent-ultra/AGENTS.md");
        assert_eq!(media.len(), 1);
        assert!(media[0].0.contains("AGENTS.md"));
        assert!(cleaned.is_empty());
    }
}
