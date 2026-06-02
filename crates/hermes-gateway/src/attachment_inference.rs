//! Infer local file paths when users request attachments but the agent omits `MEDIA:` tags.
//!
//! Candidates are collected **only from the current user message** — not from agent replies,
//! session history, or prior tool calls — to avoid sending unrelated files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use hermes_config::resolve_outbound_media_path;

static FILENAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)([A-Za-z0-9_.-]+\.(?:md|txt|pdf|docx?|xlsx?|pptx?|png|jpe?g|gif|webp|zip|csv|json|yaml|yml))",
    )
    .expect("valid filename regex")
});

static WIN_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)([A-Za-z]:[/\\][^\s`"',;)}\]]+\.(?:md|txt|pdf|docx?|xlsx?|pptx?|png|jpe?g|gif|webp|zip|csv|json|yaml|yml))"#,
    )
    .expect("valid windows path regex")
});

static ATTACHMENT_INTENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        附件
        | 以附件
        | 发我
        | 发给我
        | 发送文件
        | 重新发
        | 传给我
        | 文件发
        ",
    )
    .expect("valid attachment intent regex")
});

static SENT_CLAIM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        已经.*发
        | 已作为附件
        | 以附件形式发
        | 发给你了
        | 附件形式发给你
        ",
    )
    .expect("valid sent claim regex")
});

/// Whether the user message asks for a file attachment.
pub fn user_requests_attachment(user_text: &str) -> bool {
    ATTACHMENT_INTENT_RE.is_match(user_text)
}

/// Whether the assistant reply claims a file was already sent.
pub fn response_claims_attachment_sent(response: &str) -> bool {
    SENT_CLAIM_RE.is_match(response)
}

fn extract_filename_candidates(text: &str) -> Vec<String> {
    let mut out: Vec<String> = FILENAME_RE
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();
    out.extend(
        WIN_PATH_RE
            .captures_iter(text)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string())),
    );
    out
}

fn workspace_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::var("TERMINAL_CWD") {
        let trimmed = cwd.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.is_dir() {
                roots.push(p);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    roots.sort();
    roots.dedup();
    roots
}

fn looks_like_path(candidate: &str) -> bool {
    candidate.starts_with('/')
        || candidate.starts_with('\\')
        || candidate.starts_with("~/")
        || Path::new(candidate).has_root()
}

fn resolve_candidate(candidate: &str) -> Option<PathBuf> {
    resolve_outbound_media_path(candidate).ok()
}

fn resolve_filename_in_roots(filename: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    for root in roots {
        let joined = root.join(filename);
        if let Ok(resolved) = resolve_outbound_media_path(&joined.to_string_lossy()) {
            return Some(resolved);
        }
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.eq_ignore_ascii_case(filename)
                    && let Ok(resolved) =
                        resolve_outbound_media_path(&entry.path().to_string_lossy())
                {
                    return Some(resolved);
                }
            }
        }
    }
    None
}

fn push_unique(out: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    let key = path.to_string_lossy().to_lowercase();
    if seen.insert(key) {
        out.push(path);
    }
}

/// Infer local files to attach when `MEDIA:` tags are missing from the assistant reply.
///
/// Only inspects `user_text` for filenames and absolute paths; ignores agent replies and
/// session history so unrelated files are never bundled in.
pub fn infer_attachment_paths(user_text: &str) -> Vec<PathBuf> {
    if !user_requests_attachment(user_text) {
        return Vec::new();
    }

    let roots = workspace_roots();
    let candidates = extract_filename_candidates(user_text);

    let mut resolved = Vec::new();
    let mut seen = HashSet::new();
    for cand in candidates {
        if looks_like_path(&cand) {
            if let Some(path) = resolve_candidate(&cand) {
                push_unique(&mut resolved, &mut seen, path);
            }
            continue;
        }
        if let Some(path) = resolve_candidate(&cand) {
            push_unique(&mut resolved, &mut seen, path);
        }
        if let Some(path) = resolve_filename_in_roots(&cand, &roots) {
            push_unique(&mut resolved, &mut seen, path);
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    use hermes_core::types::Message;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn detects_attachment_intent_and_sent_claim() {
        assert!(user_requests_attachment("重新发给我 以附件形式发我"));
        assert!(user_requests_attachment(
            "将你当前路径下的readme.md文件发送给我"
        ));
        assert!(response_claims_attachment_sent("已经以附件形式发给你了 ✅"));
    }

    #[test]
    fn infers_markdown_file_from_user_message() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("AGENTS.md");
        fs::write(&file, "# agents").expect("write");
        crate::test_env::set_var("TERMINAL_CWD", dir.path());

        let paths = infer_attachment_paths("把 AGENTS.md 以附件发我");
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("AGENTS.md"));

        crate::test_env::remove_var("TERMINAL_CWD");
    }

    #[test]
    fn infers_only_from_current_user_message() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let readme = dir.path().join("readme.md");
        let agents = dir.path().join("AGENTS.md");
        fs::write(&readme, "# readme").expect("write");
        fs::write(&agents, "# agents").expect("write");
        crate::test_env::set_var("TERMINAL_CWD", dir.path());

        let _messages = vec![Message::assistant_with_tool_calls(
            None,
            vec![hermes_core::ToolCall {
                id: "tc1".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: format!(
                        r#"{{"path":"{}"}}"#,
                        agents.to_string_lossy().replace('\\', "\\\\")
                    ),
                },
                extra_content: None,
            }],
        )];

        let paths = infer_attachment_paths("将你当前路径下的readme.md文件发送给我");
        assert_eq!(paths.len(), 1, "expected readme.md from TERMINAL_CWD");
        assert!(paths[0].ends_with("readme.md"));

        crate::test_env::remove_var("TERMINAL_CWD");
    }

    #[test]
    fn does_not_trigger_on_response_claim_alone() {
        assert!(!user_requests_attachment("你好，今天天气怎么样？"));
        let paths = infer_attachment_paths("你好，今天天气怎么样？");
        assert!(paths.is_empty());
    }

    #[test]
    fn infers_windows_absolute_path_from_user_message() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("photo.jpg");
        fs::write(&file, b"fake jpeg").expect("write");
        let path_str = file.to_string_lossy().into_owned();
        let user_text = format!(r#"将这张图片发给我 {path_str}"#);

        let paths = infer_attachment_paths(&user_text);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("photo.jpg"));
    }

    #[test]
    fn infers_multiple_when_user_names_two_files() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let agents = dir.path().join("AGENTS.md");
        let readme = dir.path().join("README.md");
        fs::write(&agents, "# agents").expect("write");
        fs::write(&readme, "# readme").expect("write");
        crate::test_env::set_var("TERMINAL_CWD", dir.path());

        let paths = infer_attachment_paths("把 AGENTS.md 和 README.md 以附件发我");
        assert_eq!(paths.len(), 2);
        let names: HashSet<_> = paths
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains("AGENTS.md"));
        assert!(names.contains("README.md"));

        crate::test_env::remove_var("TERMINAL_CWD");
    }

    #[test]
    fn ignores_historical_user_messages() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let readme = dir.path().join("README.md");
        let agents = dir.path().join("AGENTS.md");
        fs::write(&readme, "# readme").expect("write");
        fs::write(&agents, "# agents").expect("write");
        crate::test_env::set_var("TERMINAL_CWD", dir.path());

        let _prior = Message::user("把 AGENTS.md 以附件发我");
        let paths = infer_attachment_paths("重新以附件形式发我");
        assert!(paths.is_empty());

        crate::test_env::remove_var("TERMINAL_CWD");
    }
}
