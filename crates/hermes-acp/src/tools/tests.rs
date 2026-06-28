#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_kind_covers_common_hermes_tools() {
        for (tool, expected) in [
            ("read_file", "read"),
            ("search_files", "search"),
            ("terminal", "execute"),
            ("patch", "edit"),
            ("write_file", "edit"),
            ("process", "execute"),
            ("web_search", "fetch"),
            ("web_extract", "fetch"),
            ("skills_list", "read"),
            ("execute_code", "execute"),
            ("todo", "other"),
            ("skill_view", "read"),
            ("browser_navigate", "fetch"),
            ("browser_click", "execute"),
            ("browser_snapshot", "read"),
            ("delegate_task", "execute"),
            ("unknown_tool", "other"),
        ] {
            assert_eq!(tool_kind(tool), expected);
        }
    }

    #[test]
    fn make_tool_call_id_uses_stable_prefix_and_unique_values() {
        let first = make_tool_call_id();
        let second = make_tool_call_id();
        assert!(first.starts_with("tc-"));
        assert!(second.starts_with("tc-"));
        assert_ne!(first, second);
    }

    #[test]
    fn tool_title_uses_human_readable_arguments() {
        assert_eq!(
            tool_title("terminal", Some(&json!({"command": "ls -la /tmp"}))),
            "ls -la /tmp"
        );
        assert_eq!(
            tool_title("read_file", Some(&json!({"path": "/etc/hosts"}))),
            "read: /etc/hosts"
        );
        assert_eq!(
            tool_title("search_files", Some(&json!({"pattern": "TODO"}))),
            "search: TODO"
        );
        assert_eq!(
            tool_title("web_search", Some(&json!({"query": "rust acp"}))),
            "search: rust acp"
        );
        assert_eq!(
            tool_title(
                "web_extract",
                Some(&json!({"urls": ["https://a.test", "https://b.test"]}))
            ),
            "extract: https://a.test (+1)"
        );
        assert_eq!(
            tool_title("browser_navigate", Some(&json!({"url": "https://x.com"}))),
            "navigate: https://x.com"
        );
        assert_eq!(
            tool_title(
                "skill_view",
                Some(&json!({"name": "github-pitfalls", "file_path": "references/api.md"}))
            ),
            "skill view (github-pitfalls/references/api.md)"
        );
        assert_eq!(
            tool_title(
                "execute_code",
                Some(&json!({"language": "rust", "code": "\nprintln!(\"hello\");"}))
            ),
            "rust: println!(\"hello\");"
        );
        assert_eq!(
            tool_title(
                "skill_manage",
                Some(&json!({"action": "patch", "name": "ops", "file_path": "ref.md"}))
            ),
            "skill patch: ops/ref.md"
        );
        assert_eq!(
            tool_title(
                "todo",
                Some(&json!({"todos": [{"id": "one", "content": "Fix ACP"}]}))
            ),
            "todo (1 item)"
        );
        assert_eq!(
            tool_title("process", Some(&json!({"action": "list"}))),
            "process list"
        );
        assert_eq!(
            tool_title(
                "delegate_task",
                Some(&json!({"tasks": [{"goal": "one"}, {"goal": "two"}]}))
            ),
            "delegate batch (2 tasks)"
        );
        assert_eq!(
            tool_title("session_search", Some(&json!({"query": "ACP"}))),
            "session search: ACP"
        );
        assert_eq!(
            tool_title("memory", Some(&json!({"action": "add", "target": "user"}))),
            "memory add: user"
        );
        assert_eq!(
            tool_title("skills_list", Some(&json!({"category": "rust"}))),
            "skills list (rust)"
        );
        assert_eq!(
            tool_title(
                "cronjob",
                Some(&json!({"action": "run", "job_id": "nightly"}))
            ),
            "cron run: nightly"
        );
    }

    #[test]
    fn terminal_titles_are_truncated() {
        let title = tool_title("terminal", Some(&json!({"command": "x".repeat(200)})));
        assert!(title.len() < 120);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn format_tool_result_renders_todo_summary_without_raw_json() {
        let result = format_tool_result(
            "todo",
            Some(
                r#"{"todos":[{"id":"a","content":"Inspect ACP","status":"completed"},{"id":"b","content":"Patch renderers","status":"in_progress"}],"summary":{"pending":0,"in_progress":1,"completed":1,"cancelled":0}}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("**Todo list**"));
        assert!(result.contains("- [x] Inspect ACP"));
        assert!(result.contains("- [~] Patch renderers"));
        assert!(result.contains("**Progress:** 1 completed, 1 in progress, 0 pending"));
        assert!(!result.contains(r#""todos""#));
    }

    #[test]
    fn format_tool_result_fences_read_file_content() {
        let result = format_tool_result(
            "read_file",
            Some(r#"{"path":"README.md","content":"1|hello\n2|world","total_lines":2}"#),
        )
        .expect("formatted");
        assert!(result.contains("Read README.md - 2 total lines"));
        assert!(result.contains("```\n1|hello\n2|world\n```"));
        assert!(!result.contains(r#""content""#));
    }

    #[test]
    fn format_tool_result_decodes_json_prefix_before_hint() {
        let result = format_tool_result(
            "search_files",
            Some(
                r#"{"total_count":2,"matches":[{"path":"README.md","line":3,"content":"TODO: fix this"},{"path":"src/app.rs","line":9,"content":"needle"}],"truncated":true}

[Hint: Results truncated. Use offset=12 to see more.]"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Search results"));
        assert!(result.contains("Found 2 matches"));
        assert!(result.contains("README.md:3"));
        assert!(result.contains("TODO: fix this"));
        assert!(result.contains("Results truncated"));
        assert!(!result.contains("[Hint:"));
    }

    #[test]
    fn format_tool_result_renders_generic_nested_json_compactly() {
        let result = format_tool_result(
            "custom_tool",
            Some(
                r#"{"success":true,"message":"ok","items":[{"id":"one","status":"done","details":{"score":0.98}},{"name":"two","url":"https://example.com"}],"content":"hidden body"}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("custom_tool completed"));
        assert!(result.contains("- **message:** ok"));
        assert!(result.contains("- **items:** 2 items"));
        assert!(result.contains("1. one"));
        assert!(result.contains("- **status:** done"));
        assert!(result.contains("hidden body"));
        assert!(!result.contains(r#""success""#));
    }

    #[test]
    fn format_tool_result_renders_web_search_results() {
        let result = format_tool_result(
            "web_search",
            Some(
                r#"{"data":{"web":[{"title":"ACP docs","url":"https://example.com/acp","description":"Agent protocol docs."},{"url":"https://example.com/zed"}]}}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Web results: 2"));
        assert!(result.contains("ACP docs - https://example.com/acp"));
        assert!(result.contains("Agent protocol docs."));
        assert!(!result.contains(r#""data""#));
    }

    #[test]
    fn format_tool_result_keeps_successful_web_extract_compact() {
        let result = format_tool_result(
            "web_extract",
            Some(
                r##"{"results":[{"url":"https://example.com","title":"Example","content":"# Intro\nThis is extracted content."}]}"##,
            ),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn format_tool_result_shows_web_extract_failures() {
        let result = format_tool_result(
            "web_extract",
            Some(
                r#"{"results":[{"url":"https://example.com","title":"Example","error":"timeout"}]}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Web extract failed for 1 URL"));
        assert!(result.contains("Example - https://example.com"));
        assert!(result.contains("timeout"));
        assert!(!result.contains(r#""results""#));
    }

    #[test]
    fn format_tool_result_renders_process_list() {
        let result = format_tool_result(
            "process",
            Some(
                r#"{"processes":[{"session_id":"p1","status":"running","pid":123,"command":"npm run dev"}]}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Processes: 1"));
        assert!(result.contains("`p1`"));
        assert!(result.contains("pid 123"));
        assert!(result.contains("npm run dev"));
        assert!(!result.contains(r#""processes""#));
    }

    #[test]
    fn format_tool_result_summarizes_delegate_children() {
        let result = format_tool_result(
            "delegate_task",
            Some(
                r#"{"results":[{"task_index":0,"status":"completed","summary":"Reviewed ACP rendering.","model":"gpt-5.5","duration_seconds":3.2,"tool_trace":[{"tool":"read_file"}]}],"total_duration_seconds":3.4}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Delegation results: 1 task in 3.4s"));
        assert!(result.contains("Task 1: completed"));
        assert!(result.contains("Reviewed ACP rendering."));
        assert!(result.contains("gpt-5.5"));
        assert!(result.contains("Tools: read_file"));
    }

    #[test]
    fn format_tool_result_renders_session_search_recent() {
        let result = format_tool_result(
            "session_search",
            Some(
                r#"{"success":true,"mode":"recent","results":[{"session_id":"s1","title":"ACP work","last_active":"2026-05-02","message_count":12,"preview":"Polished tool rendering."}],"count":1}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Recent sessions"));
        assert!(result.contains("ACP work"));
        assert!(result.contains("s1"));
        assert!(result.contains("Polished tool rendering."));
    }

    #[test]
    fn format_tool_result_memory_avoids_dumping_entries() {
        let result = format_tool_result(
            "memory",
            Some(
                r#"{"success":true,"action":"add","target":"user","entries":["private long memory"],"usage":"19/2000 chars","entry_count":1,"message":"Entry added."}"#,
            ),
        )
        .expect("formatted");
        assert!(result.contains("Memory add saved (user)"));
        assert!(result.contains("Entry added."));
        assert!(result.contains("Entries: 1"));
        assert!(!result.contains("private long memory"));
    }

    #[test]
    fn format_tool_result_renders_media_and_cron_keys() {
        let result = format_tool_result(
            "cronjob",
            Some(r#"{"success":true,"job_id":"nightly","status":"scheduled","next_run":"2026-06-07T00:00:00Z"}"#),
        )
        .expect("formatted");
        assert!(result.contains("cronjob completed"));
        assert!(result.contains("- **job_id:** nightly"));
        assert!(result.contains("- **next_run:** 2026-06-07T00:00:00Z"));
    }

    #[test]
    fn completion_status_detects_structured_failures() {
        assert_eq!(
            tool_completion_status("terminal", Some(r#"{"exit_code": 2}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("execute_code", Some(r#"{"returncode": 1}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("skill_manage", Some(r#"{"success": false}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("some_tool", Some(r#"{"ok": false}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("read_file", Some(r#"{"error": "File not found"}"#)),
            "failed"
        );
        assert_eq!(
            tool_completion_status("some_tool", Some(r#"{"error": "optional timeout"}"#)),
            "completed"
        );
        assert_eq!(
            tool_completion_status("terminal", Some("Error: pytest collected 0 items")),
            "completed"
        );
        assert_eq!(
            tool_completion_status("patch", Some("Error executing tool 'patch': boom")),
            "failed"
        );
    }
}
