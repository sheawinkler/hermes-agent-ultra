//! File tools: read, write, patch, and search

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, TerminalBackend, ToolError, ToolHandler, ToolSchema};

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::credential_guard::CredentialGuard;

const DEFAULT_READ_OFFSET: i64 = 1;
const DEFAULT_READ_LIMIT: i64 = 500;
const MAX_READ_LIMIT: i64 = 2000;
const DEFAULT_SEARCH_OFFSET: i64 = 0;
const DEFAULT_SEARCH_LIMIT: i64 = 50;
pub const DEFAULT_MAX_READ_CHARS: usize = 200_000;
pub const READ_DEDUP_STATUS_MESSAGE: &str =
    "File unchanged since last read; content omitted. Use offset/limit or edit only from known content.";

fn parse_int_param(raw: Option<&Value>, default: i64) -> i64 {
    let Some(v) = raw else {
        return default;
    };
    if let Some(i) = v.as_i64() {
        return i;
    }
    if let Some(u) = v.as_u64() {
        return i64::try_from(u).unwrap_or(i64::MAX);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<i64>().unwrap_or(default);
    }
    default
}

fn normalize_read_pagination(
    offset_raw: Option<&Value>,
    limit_raw: Option<&Value>,
) -> (Option<u64>, Option<u64>) {
    normalize_read_pagination_with_max_lines(offset_raw, limit_raw, configured_max_read_limit(None))
}

fn configured_max_read_limit(home_dir: Option<&str>) -> i64 {
    hermes_config::load_config(home_dir)
        .ok()
        .map(|config| config.tool_output.max_lines)
        .map(|max_lines| i64::try_from(max_lines).unwrap_or(i64::MAX))
        .filter(|max_lines| *max_lines > 0)
        .unwrap_or(MAX_READ_LIMIT)
}

fn normalize_read_pagination_with_max_lines(
    offset_raw: Option<&Value>,
    limit_raw: Option<&Value>,
    max_lines: i64,
) -> (Option<u64>, Option<u64>) {
    let offset = offset_raw.map(|_| {
        let normalized = parse_int_param(offset_raw, DEFAULT_READ_OFFSET).max(1);
        // Tool schema is 1-indexed while backend slicing is 0-indexed.
        normalized.saturating_sub(1) as u64
    });
    let max_lines = max_lines.max(1);
    let limit = limit_raw
        .map(|_| parse_int_param(limit_raw, DEFAULT_READ_LIMIT).clamp(1, max_lines) as u64);
    (offset, limit)
}

fn normalize_search_pagination(
    offset_raw: Option<&Value>,
    limit_raw: Option<&Value>,
) -> (Option<usize>, Option<usize>) {
    let offset =
        offset_raw.map(|_| parse_int_param(offset_raw, DEFAULT_SEARCH_OFFSET).max(0) as usize);
    let limit = limit_raw.map(|_| parse_int_param(limit_raw, DEFAULT_SEARCH_LIMIT).max(1) as usize);
    (offset, limit)
}

pub fn is_blocked_device_path(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    matches!(
        raw.as_ref(),
        "/dev/zero"
            | "/dev/random"
            | "/dev/urandom"
            | "/dev/stdin"
            | "/dev/stdout"
            | "/dev/stderr"
            | "/dev/tty"
            | "/dev/console"
            | "/dev/fd/0"
            | "/dev/fd/1"
            | "/dev/fd/2"
    ) || matches!(
        raw.as_ref(),
        "/proc/self/fd/0" | "/proc/self/fd/1" | "/proc/self/fd/2"
    ) || RegexLikeProcFd::matches(&raw)
}

struct RegexLikeProcFd;

impl RegexLikeProcFd {
    fn matches(raw: &str) -> bool {
        let Some(rest) = raw.strip_prefix("/proc/") else {
            return false;
        };
        let Some((pid, fd)) = rest.split_once("/fd/") else {
            return false;
        };
        !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) && matches!(fd, "0" | "1" | "2")
    }
}

fn expand_user_path(path: &str) -> Option<PathBuf> {
    if path == "~" {
        return std::env::var_os("HOME").map(PathBuf::from);
    }
    path.strip_prefix("~/").and_then(|rest| {
        std::env::var_os("HOME").map(|home| {
            let mut expanded = PathBuf::from(home);
            expanded.push(rest);
            expanded
        })
    })
}

fn clean_path(path: PathBuf) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(part) => cleaned.push(part),
            Component::RootDir | Component::Prefix(_) => cleaned.push(component.as_os_str()),
        }
    }
    if cleaned.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        cleaned
    }
}

/// Resolve file-tool paths the same way the Python tool facade did:
/// user expansion first, absolute paths unchanged, then live terminal cwd,
/// then `TERMINAL_CWD`, then process cwd for relative paths.
pub fn resolve_tool_path(
    input: &str,
    terminal_cwd: Option<&Path>,
    live_cwd: Option<&Path>,
) -> PathBuf {
    let expanded = expand_user_path(input).unwrap_or_else(|| PathBuf::from(input));
    if expanded.is_absolute() {
        return clean_path(expanded);
    }

    let base = live_cwd
        .or(terminal_cwd)
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    clean_path(base.join(expanded))
}

pub fn content_looks_like_internal_read_status(content: &str) -> bool {
    if !content.contains(READ_DEDUP_STATUS_MESSAGE) {
        return false;
    }
    content.chars().count() <= READ_DEDUP_STATUS_MESSAGE.chars().count() + 128
}

// ---------------------------------------------------------------------------
// ReadFileHandler
// ---------------------------------------------------------------------------

/// Tool for reading file contents via the terminal backend.
pub struct ReadFileHandler {
    backend: Arc<dyn TerminalBackend>,
}

impl ReadFileHandler {
    pub fn new(backend: Arc<dyn TerminalBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ReadFileHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        if is_blocked_device_path(Path::new(path)) {
            return Err(ToolError::ExecutionFailed(format!(
                "Refusing to read device file '{}'",
                path
            )));
        }

        CredentialGuard::new().check_read_access(Path::new(path))?;

        let (offset, limit) = normalize_read_pagination(params.get("offset"), params.get("limit"));

        let content = self
            .backend
            .read_file(path, offset, limit)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        if limit.is_none() && content.chars().count() > DEFAULT_MAX_READ_CHARS {
            return Err(ToolError::ExecutionFailed(format!(
                "Read safety limit exceeded for '{}'; use offset and limit to read smaller chunks",
                path
            )));
        }

        Ok(content)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "The file path to read"
            }),
        );
        props.insert(
            "offset".into(),
            json!({
                "type": "integer",
                "description": "Line number to start reading from (1-indexed)"
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "description": "Maximum number of lines to read"
            }),
        );

        tool_schema(
            "read_file",
            "Read file contents with optional offset and line limit. Returns the file content as a string with line numbers.",
            JsonSchema::object(props, vec!["path".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// WriteFileHandler
// ---------------------------------------------------------------------------

/// Tool for writing content to files via the terminal backend.
pub struct WriteFileHandler {
    backend: Arc<dyn TerminalBackend>,
}

impl WriteFileHandler {
    pub fn new(backend: Arc<dyn TerminalBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for WriteFileHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;

        if content_looks_like_internal_read_status(content) {
            return Err(ToolError::ExecutionFailed(
                "Write denied: content appears to be internal read_file status text".into(),
            ));
        }

        CredentialGuard::new().check_write_access(Path::new(path), content)?;

        self.backend
            .write_file(path, content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(format!(
            "Successfully wrote {} bytes to {}",
            content.len(),
            path
        ))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "The file path to write to"
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "The content to write to the file"
            }),
        );

        tool_schema(
            "write_file",
            "Write content to a file. Creates the file and parent directories if they don't exist. Overwrites existing content.",
            JsonSchema::object(props, vec!["path".into(), "content".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// PatchHandler
// ---------------------------------------------------------------------------

/// Backend trait for file patching operations.
#[async_trait]
pub trait PatchBackend: Send + Sync {
    /// Apply a patch to a file using fuzzy matching.
    async fn patch_file(
        &self,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<String, ToolError>;
}

/// Tool for patching files with fuzzy matching (find-and-replace).
pub struct PatchHandler {
    backend: Arc<dyn PatchBackend>,
}

impl PatchHandler {
    pub fn new(backend: Arc<dyn PatchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for PatchHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;

        let old_string = params
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'old_string' parameter".into()))?;

        let new_string = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        self.backend
            .patch_file(path, old_string, new_string, replace_all)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "The file path to patch"
            }),
        );
        props.insert(
            "old_string".into(),
            json!({
                "type": "string",
                "description": "The text to find in the file (fuzzy matching supported)"
            }),
        );
        props.insert(
            "new_string".into(),
            json!({
                "type": "string",
                "description": "The replacement text (use empty string to delete)"
            }),
        );
        props.insert("replace_all".into(), json!({
            "type": "boolean",
            "description": "Replace all occurrences instead of requiring a unique match (default: false)",
            "default": false
        }));

        tool_schema(
            "patch",
            "Apply targeted find-and-replace edits to a file using fuzzy matching. Minor whitespace/indentation differences won't break matching.",
            JsonSchema::object(props, vec!["path".into(), "old_string".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// SearchFilesHandler
// ---------------------------------------------------------------------------

/// Backend trait for file search operations.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Search file contents by regex pattern.
    async fn search_content(
        &self,
        pattern: &str,
        path: &str,
        file_glob: Option<&str>,
        max_results: Option<usize>,
        offset: Option<usize>,
        output_mode: Option<&str>,
        context: Option<usize>,
    ) -> Result<String, ToolError>;

    /// Search files by name (glob pattern).
    async fn search_files(
        &self,
        pattern: &str,
        path: &str,
        max_results: Option<usize>,
        offset: Option<usize>,
    ) -> Result<String, ToolError>;
}

/// Tool for searching files by content or filename.
pub struct SearchFilesHandler {
    backend: Arc<dyn SearchBackend>,
}

impl SearchFilesHandler {
    pub fn new(backend: Arc<dyn SearchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for SearchFilesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'pattern' parameter".into()))?;

        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let target = params
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("content");

        let file_glob = params.get("file_glob").and_then(|v| v.as_str());

        let (offset, max_results) =
            normalize_search_pagination(params.get("offset"), params.get("limit"));
        let output_mode = params.get("output_mode").and_then(|v| v.as_str());
        let context = params
            .get("context")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        match target {
            "content" => {
                self.backend
                    .search_content(
                        pattern,
                        path,
                        file_glob,
                        max_results,
                        offset,
                        output_mode,
                        context,
                    )
                    .await
            }
            "files" => {
                self.backend
                    .search_files(pattern, path, max_results, offset)
                    .await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown target: '{}'. Use 'content' or 'files'.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "pattern".into(),
            json!({
                "type": "string",
                "description": "Regex pattern to search for (content) or glob pattern (files)"
            }),
        );
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Directory or file to search in (default: '.')"
            }),
        );
        props.insert("target".into(), json!({
            "type": "string",
            "description": "Search target: 'content' for file contents or 'files' for filenames",
            "enum": ["content", "files"],
            "default": "content"
        }));
        props.insert(
            "file_glob".into(),
            json!({
                "type": "string",
                "description": "Filter files by glob pattern when searching content (e.g. '*.py')"
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "description": "Maximum number of results to return"
            }),
        );
        props.insert(
            "offset".into(),
            json!({
                "type": "integer",
                "description": "Starting index for paginated search results"
            }),
        );
        props.insert(
            "output_mode".into(),
            json!({
                "type": "string",
                "description": "Search output format when target='content'",
                "enum": ["content", "files_only", "count"],
                "default": "content"
            }),
        );
        props.insert(
            "context".into(),
            json!({
                "type": "integer",
                "description": "Include this many surrounding lines around each content match"
            }),
        );

        tool_schema(
            "search_files",
            "Search file contents or find files by name. Uses ripgrep-backed regex search for content and glob patterns for filenames.",
            JsonSchema::object(props, vec!["pattern".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{AgentError, CommandOutput};

    struct MockBackend;
    #[async_trait]
    impl TerminalBackend for MockBackend {
        async fn execute_command(
            &self,
            _cmd: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _bg: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
        async fn read_file(
            &self,
            path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(format!("contents of {}", path))
        }
        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn test_read_file_handler() {
        let handler = ReadFileHandler::new(Arc::new(MockBackend));
        let result = handler
            .execute(json!({"path": "/tmp/test.txt"}))
            .await
            .unwrap();
        assert!(result.contains("/tmp/test.txt"));
    }

    #[tokio::test]
    async fn test_write_file_handler() {
        let handler = WriteFileHandler::new(Arc::new(MockBackend));
        let result = handler
            .execute(json!({"path": "/tmp/test.txt", "content": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("Successfully wrote"));
    }

    #[tokio::test]
    async fn test_read_file_schema() {
        let handler = ReadFileHandler::new(Arc::new(MockBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "read_file");
    }

    #[tokio::test]
    async fn test_write_file_schema() {
        let handler = WriteFileHandler::new(Arc::new(MockBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "write_file");
    }

    #[test]
    fn read_pagination_normalizes_invalid_values() {
        let default_max_lines = hermes_config::DEFAULT_TOOL_OUTPUT_MAX_LINES as i64;
        let (offset, limit) = normalize_read_pagination_with_max_lines(
            Some(&json!(0)),
            Some(&json!(0)),
            default_max_lines,
        );
        assert_eq!(offset, Some(0));
        assert_eq!(limit, Some(1));

        let (offset, limit) = normalize_read_pagination_with_max_lines(
            Some(&json!(-10)),
            Some(&json!(-5)),
            default_max_lines,
        );
        assert_eq!(offset, Some(0));
        assert_eq!(limit, Some(1));

        let (offset, limit) = normalize_read_pagination_with_max_lines(
            Some(&json!("bad")),
            Some(&json!("bad")),
            default_max_lines,
        );
        assert_eq!(offset, Some(0));
        assert_eq!(limit, Some(500));

        let (offset, limit) = normalize_read_pagination_with_max_lines(
            Some(&json!(2)),
            Some(&json!(999_999)),
            default_max_lines,
        );
        assert_eq!(offset, Some(1));
        assert_eq!(limit, Some(2000));
    }

    #[test]
    fn read_pagination_clamps_to_configured_tool_output_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("config.yaml"),
            "tool_output:\n  max_lines: 50\n",
        )
        .expect("config.yaml");

        let home = tmp.path().to_str().expect("utf-8 tempdir");
        let max_lines = configured_max_read_limit(Some(home));
        let (offset, limit) = normalize_read_pagination_with_max_lines(
            Some(&json!(1)),
            Some(&json!(1000)),
            max_lines,
        );

        assert_eq!(max_lines, 50);
        assert_eq!(offset, Some(0));
        assert_eq!(limit, Some(50));
    }

    #[test]
    fn search_pagination_normalizes_invalid_values() {
        let (offset, limit) = normalize_search_pagination(Some(&json!(-10)), Some(&json!(-5)));
        assert_eq!(offset, Some(0));
        assert_eq!(limit, Some(1));

        let (offset, limit) = normalize_search_pagination(Some(&json!("bad")), Some(&json!("bad")));
        assert_eq!(offset, Some(0));
        assert_eq!(limit, Some(50));

        let (offset, limit) = normalize_search_pagination(Some(&json!(3)), Some(&json!(0)));
        assert_eq!(offset, Some(3));
        assert_eq!(limit, Some(1));
    }

    #[test]
    fn blocked_device_detection_matches_python_guard() {
        for path in [
            "/dev/zero",
            "/dev/random",
            "/dev/urandom",
            "/dev/stdin",
            "/dev/tty",
            "/dev/console",
            "/dev/stdout",
            "/dev/stderr",
            "/dev/fd/0",
            "/dev/fd/1",
            "/dev/fd/2",
            "/proc/self/fd/0",
            "/proc/12345/fd/2",
        ] {
            assert!(is_blocked_device_path(Path::new(path)), "{path}");
        }

        for path in [
            "/dev/null",
            "/dev/sda1",
            "/proc/self/fd/3",
            "/proc/self/maps",
            "/tmp/test.py",
            "/home/user/.bashrc",
        ] {
            assert!(!is_blocked_device_path(Path::new(path)), "{path}");
        }
    }

    #[tokio::test]
    async fn read_file_handler_rejects_device_paths_before_backend_io() {
        let handler = ReadFileHandler::new(Arc::new(MockBackend));
        let err = handler
            .execute(json!({"path": "/dev/zero"}))
            .await
            .expect_err("device path should be rejected");
        assert!(err.to_string().contains("device file"));
    }

    #[tokio::test]
    async fn read_file_handler_rejects_oversized_unbounded_reads() {
        struct BigBackend;
        #[async_trait]
        impl TerminalBackend for BigBackend {
            async fn execute_command(
                &self,
                _cmd: &str,
                _timeout: Option<u64>,
                _workdir: Option<&str>,
                _bg: bool,
                _pty: bool,
            ) -> Result<CommandOutput, AgentError> {
                unreachable!()
            }
            async fn read_file(
                &self,
                _path: &str,
                _offset: Option<u64>,
                _limit: Option<u64>,
            ) -> Result<String, AgentError> {
                Ok("x".repeat(DEFAULT_MAX_READ_CHARS + 1))
            }
            async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
                unreachable!()
            }
            async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
                Ok(true)
            }
        }

        let handler = ReadFileHandler::new(Arc::new(BigBackend));
        let err = handler
            .execute(json!({"path": "/tmp/huge.txt"}))
            .await
            .expect_err("oversized read should be rejected");
        let msg = err.to_string();
        assert!(msg.contains("safety limit"));
        assert!(msg.contains("offset and limit"));

        let bounded = handler
            .execute(json!({"path": "/tmp/huge.txt", "limit": 1}))
            .await
            .expect("explicit limit delegates to backend");
        assert_eq!(bounded.len(), DEFAULT_MAX_READ_CHARS + 1);
    }

    #[test]
    fn internal_read_status_write_guard_is_status_dominated_only() {
        assert!(content_looks_like_internal_read_status(
            READ_DEDUP_STATUS_MESSAGE
        ));
        assert!(content_looks_like_internal_read_status(&format!(
            "Note: {READ_DEDUP_STATUS_MESSAGE}\n\n(continuing.)"
        )));
        let documented = format!(
            "# Skill reference\n\n    {READ_DEDUP_STATUS_MESSAGE}\n\n{}",
            "This is documentation content. ".repeat(200)
        );
        assert!(!content_looks_like_internal_read_status(&documented));
    }

    #[tokio::test]
    async fn write_file_handler_rejects_internal_read_status_text() {
        let handler = WriteFileHandler::new(Arc::new(MockBackend));
        let err = handler
            .execute(json!({"path": "/tmp/out.txt", "content": READ_DEDUP_STATUS_MESSAGE}))
            .await
            .expect_err("status text should be rejected");
        assert!(err.to_string().contains("internal read_file status text"));
    }

    #[test]
    fn resolve_tool_path_prefers_live_cwd_then_terminal_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let start = tmp.path().join("start");
        let live = tmp.path().join("worktree");
        std::fs::create_dir_all(&start).expect("start");
        std::fs::create_dir_all(&live).expect("live");

        assert_eq!(
            resolve_tool_path("nested/file.txt", Some(&start), Some(&live)),
            live.join("nested/file.txt")
        );
        assert_eq!(
            resolve_tool_path("a/../b/file.txt", Some(&start), None),
            start.join("b/file.txt")
        );

        let absolute = tmp.path().join("already-absolute.txt");
        assert_eq!(
            resolve_tool_path(absolute.to_str().unwrap(), Some(&start), Some(&live)),
            absolute
        );
    }
}
