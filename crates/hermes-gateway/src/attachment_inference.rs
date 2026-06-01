//! Infer local file paths when users request attachments but the agent omits `MEDIA:` tags.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use hermes_config::resolve_outbound_media_path;
use hermes_core::types::{Message, MessageRole};

static FILENAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)([\w.-]+\.(?:md|txt|pdf|docx?|xlsx?|pptx?|png|jpe?g|gif|webp|zip|csv|json|yaml|yml))",
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

fn paths_from_tool_calls(messages: &[Message]) -> Vec<String> {
    let mut paths = Vec::new();
    for msg in messages {
        if msg.role == MessageRole::Assistant {
            let Some(tool_calls) = msg.tool_calls.as_ref() else {
                continue;
            };
            for tc in tool_calls {
                let name = tc.function.name.as_str();
                if !matches!(name, "read_file" | "write_file" | "patch" | "search_files") {
                    continue;
                }
                let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                else {
                    continue;
                };
                for key in ["path", "file_path", "file"] {
                    if let Some(p) = args.get(key).and_then(|v| v.as_str()) {
                        let trimmed = p.trim();
                        if !trimmed.is_empty() {
                            paths.push(trimmed.to_string());
                        }
                    }
                }
            }
        } else if msg.role == MessageRole::Tool {
            if let Some(content) = &msg.content {
                paths.extend(extract_filename_candidates(content));
                for line in content.lines() {
                    let trimmed = line.trim();
                    if looks_like_path(trimmed) {
                        paths.push(trimmed.to_string());
                    }
                }
            }
        }
    }
    paths
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
pub fn infer_attachment_paths(
    user_text: &str,
    response: &str,
    session_messages: &[Message],
) -> Vec<PathBuf> {
    let wants_attachment =
        user_requests_attachment(user_text) || response_claims_attachment_sent(response);
    if !wants_attachment {
        return Vec::new();
    }

    let roots = workspace_roots();
    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(extract_filename_candidates(user_text));
    candidates.extend(extract_filename_candidates(response));
    for msg in session_messages {
        if msg.role != MessageRole::User {
            continue;
        }
        if let Some(content) = &msg.content {
            candidates.extend(extract_filename_candidates(content));
        }
    }
    candidates.extend(paths_from_tool_calls(session_messages));

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

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn detects_attachment_intent_and_sent_claim() {
        assert!(user_requests_attachment("重新发给我 以附件形式发我"));
        assert!(response_claims_attachment_sent("已经以附件形式发给你了 ✅"));
    }

    #[test]
    fn infers_markdown_file_from_user_message() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("AGENTS.md");
        fs::write(&file, "# agents").expect("write");
        crate::test_env::set_var("TERMINAL_CWD", dir.path());

        let paths = infer_attachment_paths("把 AGENTS.md 以附件发我", "好的", &[]);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("AGENTS.md"));

        crate::test_env::remove_var("TERMINAL_CWD");
    }

    #[test]
    fn infers_from_read_file_tool_call() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("notes.txt");
        fs::write(&file, "hello").expect("write");
        let path_str = file.to_string_lossy().into_owned();
        let path_json = path_str.replace('\\', "\\\\");
        crate::test_env::set_var("TERMINAL_CWD", dir.path());

        let messages = vec![Message::assistant_with_tool_calls(
            None,
            vec![hermes_core::ToolCall {
                id: "tc1".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: format!(r#"{{"path":"{path_json}"}}"#),
                },
                extra_content: None,
            }],
        )];

        let paths = infer_attachment_paths("重新以附件形式发我", "已经发给你了", &messages);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("notes.txt"));

        crate::test_env::remove_var("TERMINAL_CWD");
    }

    #[test]
    fn infers_windows_absolute_path_from_response() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("AGENTS.md");
        fs::write(&file, "# agents").expect("write");
        let path_str = file.to_string_lossy().into_owned();
        let response = format!("路径 {path_str}");

        let paths = infer_attachment_paths(
            "重新以附件形式发我",
            &response,
            &[],
        );
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("AGENTS.md"));
    }
}
