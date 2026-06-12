//! Tool batch parallelism gating — parity with Python `agent/tool_dispatch_helpers.py`.

use std::path::{Path, PathBuf};

use hermes_core::ToolCall;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// ParallelMode — registered once per tool, read on every dispatch call
// ---------------------------------------------------------------------------

/// Describes how a tool participates in parallel batch dispatch.
///
/// Assigned at registration time; eliminates the per-call HashSet rebuild
/// that `should_parallelize_tool_batch` previously performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParallelMode {
    /// Never run this tool concurrently (interactive / session-owning tools).
    Serial,
    /// Always safe to run concurrently with other `Always` tools.
    Always,
    /// Safe when the `path` argument does not overlap with other path-scoped tools.
    PathScoped,
    /// Safe when the `command` argument is non-destructive.
    CommandScoped,
    /// Default: cannot be parallelised until proven safe (conservative).
    #[default]
    Unknown,
}

/// Infer `ParallelMode` for a tool name at registration time.
///
/// Keeps the static tables as the single source of truth; the mode is
/// stored in `ToolEntry` so lookup is O(1) per dispatch call.
pub fn infer_parallel_mode(name: &str) -> ParallelMode {
    if NEVER_PARALLEL_TOOLS.contains(&name) || name.starts_with("browser_") {
        return ParallelMode::Serial;
    }
    if PATH_SCOPED_TOOLS.contains(&name) {
        return ParallelMode::PathScoped;
    }
    if name == "terminal" {
        return ParallelMode::CommandScoped;
    }
    if PARALLEL_SAFE_TOOLS.contains(&name) {
        return ParallelMode::Always;
    }
    ParallelMode::Unknown
}

/// Tools that must never run concurrently (interactive / user-facing).
pub const NEVER_PARALLEL_TOOLS: &[&str] = &["clarify"];

/// Browser automation shares one CDP / agent-browser session per task — never parallel.
pub fn is_browser_tool(name: &str) -> bool {
    name.starts_with("browser_")
}

/// Read-only tools with no shared mutable session state.
const PARALLEL_SAFE_TOOLS: &[&str] = &[
    "ha_get_state",
    "ha_list_entities",
    "ha_list_services",
    "read_file",
    "search_files",
    "session_search",
    "skill_view",
    "skills_list",
    "vision_analyze",
    "web_extract",
    "web_search",
];

/// File tools can run concurrently when they target independent paths.
const PATH_SCOPED_TOOLS: &[&str] = &["read_file", "write_file", "patch"];

static DESTRUCTIVE_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|\s|&&|\|\||;|`)(?:rm\s|rmdir\s|cp\s|install\s|mv\s|sed\s+-i|truncate\s|dd\s|shred\s|git\s+(?:reset|clean|checkout)\s)",
    )
    .expect("destructive patterns regex")
});

static REDIRECT_OVERWRITE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^>]>[^>]|^>[^>]").expect("redirect overwrite regex"));

/// Heuristic: does this terminal command look like it modifies/deletes files?
pub fn is_destructive_command(cmd: &str) -> bool {
    if cmd.is_empty() {
        return false;
    }
    DESTRUCTIVE_PATTERNS.is_match(cmd) || REDIRECT_OVERWRITE.is_match(cmd)
}

fn parse_tool_args(arguments: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    value.is_object().then_some(value)
}

/// Return the normalized file target for path-scoped tools.
pub fn extract_parallel_scope_path(tool_name: &str, function_args: &Value) -> Option<PathBuf> {
    if !PATH_SCOPED_TOOLS.contains(&tool_name) {
        return None;
    }
    let raw_path = function_args.get("path")?.as_str()?.trim();
    if raw_path.is_empty() {
        return None;
    }
    let expanded = Path::new(raw_path);
    if expanded.is_absolute() {
        return std::fs::canonicalize(expanded).ok().or_else(|| {
            Some(PathBuf::from(
                std::env::current_dir()
                    .ok()?
                    .join(expanded)
                    .to_string_lossy()
                    .into_owned(),
            ))
        });
    }
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join(expanded))
}

/// Return true when two paths may refer to the same subtree.
pub fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left_parts: Vec<_> = left.components().collect();
    let right_parts: Vec<_> = right.components().collect();
    if left_parts.is_empty() || right_parts.is_empty() {
        return left_parts.is_empty() == right_parts.is_empty() && !left_parts.is_empty();
    }
    let common_len = left_parts.len().min(right_parts.len());
    left_parts[..common_len] == right_parts[..common_len]
}

/// Return true when a tool-call batch is safe to run concurrently.
///
/// Uses `infer_parallel_mode` for each tool name — O(n) single pass, no
/// per-call HashSet builds.  This is equivalent to the previous logic but
/// removes the three `HashSet::new()` allocations per invocation.
pub fn should_parallelize_tool_batch(tool_calls: &[ToolCall]) -> bool {
    if tool_calls.len() <= 1 {
        return false;
    }

    let mut reserved_paths: Vec<PathBuf> = Vec::new();

    for tc in tool_calls {
        let tool_name = tc.function.name.as_str();

        match infer_parallel_mode(tool_name) {
            ParallelMode::Serial => return false,
            ParallelMode::Always => {} // unconditionally safe
            ParallelMode::PathScoped => {
                let Some(function_args) = parse_tool_args(&tc.function.arguments) else {
                    tracing::debug!(
                        tool = tool_name,
                        "could not parse tool args — defaulting to sequential"
                    );
                    return false;
                };
                let Some(scoped_path) = extract_parallel_scope_path(tool_name, &function_args)
                else {
                    return false;
                };
                if reserved_paths
                    .iter()
                    .any(|existing| paths_overlap(&scoped_path, existing))
                {
                    return false;
                }
                reserved_paths.push(scoped_path);
            }
            ParallelMode::CommandScoped => {
                let Some(function_args) = parse_tool_args(&tc.function.arguments) else {
                    tracing::debug!(
                        tool = tool_name,
                        "could not parse tool args — defaulting to sequential"
                    );
                    return false;
                };
                if let Some(cmd) = function_args.get("command").and_then(|v| v.as_str()) {
                    if is_destructive_command(cmd) {
                        return false;
                    }
                }
                // Terminal shares session state; serial even when non-destructive.
                return false;
            }
            ParallelMode::Unknown => return false,
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{FunctionCall, ToolCall};

    fn tc(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "1".to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
            extra_content: None,
        }
    }

    #[test]
    fn single_tool_not_parallel() {
        assert!(!should_parallelize_tool_batch(&[tc(
            "read_file",
            r#"{"path":"a.txt"}"#
        )]));
    }

    #[test]
    fn clarify_blocks_parallel() {
        let batch = [
            tc("read_file", r#"{"path":"a.txt"}"#),
            tc("clarify", r#"{"question":"?"}"#),
        ];
        assert!(!should_parallelize_tool_batch(&batch));
    }

    #[test]
    fn overlapping_paths_not_parallel() {
        let batch = [
            tc("read_file", r#"{"path":"src/main.rs"}"#),
            tc("write_file", r#"{"path":"src/lib.rs"}"#),
        ];
        assert!(!should_parallelize_tool_batch(&batch));
    }

    #[test]
    fn independent_read_files_parallel() {
        let batch = [
            tc("read_file", r#"{"path":"a.txt"}"#),
            tc("read_file", r#"{"path":"b.txt"}"#),
        ];
        assert!(should_parallelize_tool_batch(&batch));
    }

    #[test]
    fn browser_tools_not_parallel() {
        let batch = [
            tc("browser_snapshot", "{}"),
            tc("browser_navigate", r#"{"url":"https://example.com"}"#),
        ];
        assert!(!should_parallelize_tool_batch(&batch));
    }

    #[test]
    fn destructive_terminal_not_parallel() {
        let batch = [
            tc("terminal", r#"{"command":"rm -rf /tmp/x"}"#),
            tc("read_file", r#"{"path":"a.txt"}"#),
        ];
        assert!(!should_parallelize_tool_batch(&batch));
    }
}
