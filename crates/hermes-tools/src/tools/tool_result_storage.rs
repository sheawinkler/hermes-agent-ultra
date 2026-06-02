use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hermes_core::{
    tool_schema, BudgetConfig, JsonSchema, ToolError, ToolHandler, ToolResult, ToolSchema,
};
use indexmap::IndexMap;
use serde_json::{json, Value};
use uuid::Uuid;

pub const PERSISTED_OUTPUT_TAG: &str = "<persisted-output>";
pub const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";
pub const STORAGE_DIR: &str = "/tmp/hermes-results";
pub const HEREDOC_MARKER: &str = "HERMES_PERSIST_EOF";
pub const DEFAULT_PREVIEW_SIZE_CHARS: usize = 2_000;
const BUDGET_ENFORCEMENT_TOOL: &str = "__budget_enforcement__";

#[cfg(test)]
pub(crate) static STORAGE_ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub content: String,
    pub has_more: bool,
}

#[derive(Clone, Default)]
pub struct ToolResultStorageHandler {
    store: Arc<Mutex<HashMap<String, String>>>,
}

pub fn char_count(content: &str) -> usize {
    content.chars().count()
}

fn byte_index_at_char(content: &str, max_chars: usize) -> usize {
    if max_chars == 0 {
        return 0;
    }
    content
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(content.len())
}

pub fn generate_preview(content: &str, max_chars: usize) -> Preview {
    if max_chars == 0 {
        return Preview {
            content: String::new(),
            has_more: !content.is_empty(),
        };
    }
    if char_count(content) <= max_chars {
        return Preview {
            content: content.to_string(),
            has_more: false,
        };
    }

    let end = byte_index_at_char(content, max_chars);
    let mut preview = &content[..end];
    if let Some(last_newline) = preview.rfind('\n') {
        let newline_end = last_newline + '\n'.len_utf8();
        if char_count(&preview[..newline_end]) > max_chars / 2 {
            preview = &preview[..newline_end];
        }
    }

    Preview {
        content: preview.to_string(),
        has_more: true,
    }
}

pub fn heredoc_marker(content: &str) -> String {
    if !content.contains(HEREDOC_MARKER) {
        return HEREDOC_MARKER.to_string();
    }
    loop {
        let candidate = format!("HERMES_PERSIST_{}", Uuid::new_v4().simple());
        if !content.contains(&candidate) {
            return candidate;
        }
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn build_sandbox_write_command(content: &str, remote_path: &str) -> String {
    let marker = heredoc_marker(content);
    let storage_dir = Path::new(remote_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    format!(
        "mkdir -p {} && cat > {} << '{}'\n{}\n{}",
        shell_quote(&storage_dir),
        shell_quote(remote_path),
        marker,
        content,
        marker
    )
}

pub fn resolve_storage_dir() -> PathBuf {
    std::env::var_os("HERMES_TOOL_RESULT_STORAGE_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(STORAGE_DIR))
}

fn safe_file_stem(tool_use_id: &str) -> String {
    let sanitized: String = tool_use_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('.').trim_matches('_');
    if trimmed.is_empty() {
        "tool_result".to_string()
    } else {
        trimmed.to_string()
    }
}

fn persisted_file_path(tool_use_id: &str) -> PathBuf {
    resolve_storage_dir().join(format!("{}.txt", safe_file_stem(tool_use_id)))
}

fn write_to_local_storage(content: &str, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

fn size_label(original_size: usize) -> String {
    let kb = original_size as f64 / 1024.0;
    if kb >= 1024.0 {
        format!("{:.1} MB", kb / 1024.0)
    } else {
        format!("{:.1} KB", kb)
    }
}

fn format_count(value: usize) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (idx, ch) in raw.chars().enumerate() {
        if idx > 0 && (raw.len() - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn build_persisted_message(
    preview: &str,
    has_more: bool,
    original_size: usize,
    file_path: &Path,
) -> String {
    let mut msg = String::new();
    msg.push_str(PERSISTED_OUTPUT_TAG);
    msg.push('\n');
    msg.push_str(&format!(
        "This tool result was too large ({} characters, {}).\n",
        format_count(original_size),
        size_label(original_size)
    ));
    msg.push_str(&format!("Full output saved to: {}\n", file_path.display()));
    msg.push_str(
        "Use the read_file tool with offset and limit to access specific sections of this output.\n\n",
    );
    msg.push_str(&format!("Preview (first {} chars):\n", char_count(preview)));
    msg.push_str(preview);
    if has_more {
        msg.push_str("\n...");
    }
    msg.push('\n');
    msg.push_str(PERSISTED_OUTPUT_CLOSING_TAG);
    msg
}

pub fn fallback_truncate(content: &str, preview_size_chars: usize) -> String {
    let preview = generate_preview(content, preview_size_chars);
    format!(
        "{}\n\n[Truncated: tool response was {} chars. Full output could not be saved to disk.]",
        preview.content,
        format_count(char_count(content))
    )
}

pub fn default_threshold_for_tool(tool_name: &str, default_threshold: usize) -> Option<usize> {
    match tool_name {
        "read_file" => None,
        _ => Some(default_threshold),
    }
}

pub fn maybe_persist_tool_result(
    content: &str,
    tool_name: &str,
    tool_use_id: &str,
    threshold: Option<usize>,
) -> String {
    let Some(threshold) = threshold else {
        return content.to_string();
    };
    let content_size = char_count(content);
    if content_size == 0 || content_size <= threshold {
        return content.to_string();
    }

    let file_path = persisted_file_path(tool_use_id);
    let preview = generate_preview(content, DEFAULT_PREVIEW_SIZE_CHARS);
    match write_to_local_storage(content, &file_path) {
        Ok(()) => {
            build_persisted_message(&preview.content, preview.has_more, content_size, &file_path)
        }
        Err(err) => {
            tracing::warn!(
                tool_name,
                tool_use_id,
                path = %file_path.display(),
                error = %err,
                "failed to persist oversized tool result; falling back to inline truncation"
            );
            fallback_truncate(content, DEFAULT_PREVIEW_SIZE_CHARS)
        }
    }
}

pub fn is_persisted_output(content: &str) -> bool {
    content.contains(PERSISTED_OUTPUT_TAG) && content.contains(PERSISTED_OUTPUT_CLOSING_TAG)
}

pub fn enforce_turn_budget(results: &mut [ToolResult], budget: &BudgetConfig) {
    let mut total_chars: usize = results.iter().map(|r| char_count(&r.content)).sum();
    if total_chars <= budget.max_aggregate_chars {
        return;
    }

    let mut candidates: Vec<(usize, usize)> = results
        .iter()
        .enumerate()
        .filter(|(_, result)| !is_persisted_output(&result.content))
        .map(|(idx, result)| (idx, char_count(&result.content)))
        .collect();
    candidates.sort_by(|(_, left), (_, right)| right.cmp(left));

    for (idx, original_size) in candidates {
        if total_chars <= budget.max_aggregate_chars {
            break;
        }
        let replacement = maybe_persist_tool_result(
            &results[idx].content,
            BUDGET_ENFORCEMENT_TOOL,
            &results[idx].tool_call_id,
            Some(0),
        );
        if replacement != results[idx].content {
            total_chars = total_chars
                .saturating_sub(original_size)
                .saturating_add(char_count(&replacement));
            results[idx].content = replacement;
        }
    }
}

#[async_trait]
impl ToolHandler for ToolResultStorageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("get");
        let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
        if key.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'key'".into()));
        }
        match action {
            "set" => {
                let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
                self.store
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .insert(key.to_string(), value.to_string());
                Ok(json!({"status":"stored","key":key}).to_string())
            }
            _ => {
                let value = self
                    .store
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .get(key)
                    .cloned();
                Ok(json!({"key":key,"value":value}).to_string())
            }
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({"type":"string","enum":["get","set"]}),
        );
        props.insert("key".into(), json!({"type":"string"}));
        props.insert("value".into(), json!({"type":"string"}));
        tool_schema(
            "tool_result_storage",
            "Persist/retrieve tool results by key.",
            JsonSchema::object(props, vec!["key".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_uses_newline_boundary_only_after_halfway() {
        let late = format!("{}\n{}", "a".repeat(1500), "b".repeat(600));
        let preview = generate_preview(&late, 2000);
        assert_eq!(preview.content, format!("{}\n", "a".repeat(1500)));
        assert!(preview.has_more);

        let early = format!("{}\n{}", "a".repeat(100), "b".repeat(3000));
        let preview = generate_preview(&early, 2000);
        assert_eq!(char_count(&preview.content), 2000);
        assert!(preview.has_more);
    }

    #[test]
    fn preview_preserves_unicode_boundaries() {
        let content = "日本語テスト ".repeat(10_000);
        let preview = generate_preview(&content, 2_000);
        assert!(preview.content.contains("日本語テスト"));
        assert!(preview.has_more);
        assert!(std::str::from_utf8(preview.content.as_bytes()).is_ok());
    }

    #[test]
    fn heredoc_marker_avoids_content_collision() {
        assert_eq!(heredoc_marker("normal content"), HEREDOC_MARKER);
        let marker = heredoc_marker(&format!("content {HEREDOC_MARKER} embedded"));
        assert_ne!(marker, HEREDOC_MARKER);
        assert!(marker.starts_with("HERMES_PERSIST_"));
    }

    #[test]
    fn sandbox_write_command_quotes_paths_and_uses_parent_dir() {
        let command = build_sandbox_write_command("hello", "/tmp/hermes results/abc file.txt");
        assert!(command.contains("mkdir -p '/tmp/hermes results'"));
        assert!(command.contains("cat > '/tmp/hermes results/abc file.txt'"));
        assert!(command.contains(HEREDOC_MARKER));

        let malicious = build_sandbox_write_command("content", "/tmp/x; rm -rf /; echo .txt");
        assert!(malicious.contains("'/tmp/x; rm -rf /; echo .txt'"));
    }

    #[test]
    fn persisted_message_has_contract_shape() {
        let msg = build_persisted_message(
            "first 100 chars...",
            true,
            50_000,
            Path::new("/tmp/hermes-results/test123.txt"),
        );
        assert!(msg.starts_with(PERSISTED_OUTPUT_TAG));
        assert!(msg.ends_with(PERSISTED_OUTPUT_CLOSING_TAG));
        assert!(msg.contains("50,000 characters"));
        assert!(msg.contains("/tmp/hermes-results/test123.txt"));
        assert!(msg.contains("read_file"));
        assert!(msg.contains("first 100 chars..."));
        assert!(msg.contains("KB"));

        let large = build_persisted_message("x", true, 2_000_000, Path::new("/tmp/big.txt"));
        assert!(large.contains("MB"));
    }

    #[test]
    fn maybe_persist_writes_full_output_and_replaces_context() {
        let _guard = STORAGE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_TOOL_RESULT_STORAGE_DIR", tmp.path());

        let content = format!("DISTINCTIVE_START_MARKER{}", "x".repeat(60_000));
        let result = maybe_persist_tool_result(&content, "terminal", "tc_456", Some(30_000));
        assert!(result.contains(PERSISTED_OUTPUT_TAG));
        assert!(result.contains("tc_456.txt"));
        assert!(result.contains("DISTINCTIVE_START_MARKER"));
        assert!(result.len() < content.len());
        assert_eq!(
            fs::read_to_string(tmp.path().join("tc_456.txt")).unwrap(),
            content
        );

        std::env::remove_var("HERMES_TOOL_RESULT_STORAGE_DIR");
    }

    #[test]
    fn threshold_none_never_persists_and_zero_forces_persist() {
        let _guard = STORAGE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_TOOL_RESULT_STORAGE_DIR", tmp.path());

        let large = "x".repeat(60_000);
        assert_eq!(
            maybe_persist_tool_result(&large, "read_file", "tc_rf", None),
            large
        );

        let forced =
            maybe_persist_tool_result("even short content", "terminal", "tc_zero", Some(0));
        assert!(forced.contains(PERSISTED_OUTPUT_TAG));
        assert_eq!(
            fs::read_to_string(tmp.path().join("tc_zero.txt")).unwrap(),
            "even short content"
        );

        std::env::remove_var("HERMES_TOOL_RESULT_STORAGE_DIR");
    }

    #[test]
    fn enforce_turn_budget_spills_largest_first_and_skips_existing() {
        let _guard = STORAGE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_TOOL_RESULT_STORAGE_DIR", tmp.path());

        let mut results = vec![
            ToolResult::ok(
                "t0",
                format!(
                    "{PERSISTED_OUTPUT_TAG}\nalready persisted\n{PERSISTED_OUTPUT_CLOSING_TAG}"
                ),
            ),
            ToolResult::ok("t1", "a".repeat(80_000)),
            ToolResult::ok("t2", "b".repeat(130_000)),
        ];
        enforce_turn_budget(
            &mut results,
            &BudgetConfig {
                max_result_size_chars: 100_000,
                max_aggregate_chars: 200_000,
            },
        );
        assert!(results[0].content.contains("already persisted"));
        assert!(!results[1].content.contains(PERSISTED_OUTPUT_TAG));
        assert!(results[2].content.contains(PERSISTED_OUTPUT_TAG));
        assert_eq!(
            fs::read_to_string(tmp.path().join("t2.txt")).unwrap(),
            "b".repeat(130_000)
        );

        std::env::remove_var("HERMES_TOOL_RESULT_STORAGE_DIR");
    }
}
