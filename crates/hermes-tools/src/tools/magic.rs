//! Rust-native "magic harness" tools: hash-stable edits, URI resources,
//! conflict transactions, lightweight LSP/DAP, advisor guards, subagent
//! workspaces, eval kernels, output minimization, inheritance, and benchmarks.

use async_trait::async_trait;
use chrono::Utc;
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::credential_guard::CredentialGuard;
use crate::tools::file::content_looks_like_internal_read_status;

const MAX_RESOURCE_CHARS: usize = 200_000;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_SCAN_FILES: usize = 600;
const MAGIC_VERSION: &str = "2026-07-05";

#[derive(Debug, Clone)]
pub struct MagicState {
    root: PathBuf,
}

impl MagicState {
    pub fn new(root: PathBuf) -> Self {
        let state = Self { root };
        state.ensure_dirs();
        state
    }

    fn ensure_dirs(&self) {
        for dir in [
            self.root.clone(),
            self.transactions_dir(),
            self.agents_dir(),
            self.kernels_dir(),
        ] {
            let _ = fs::create_dir_all(dir);
        }
    }

    fn transactions_dir(&self) -> PathBuf {
        self.root.join("transactions")
    }

    fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }

    fn kernels_dir(&self) -> PathBuf {
        self.root.join("kernels")
    }

    fn rules_path(&self) -> PathBuf {
        self.root.join("stream_rules.json")
    }

    fn import_path(&self) -> PathBuf {
        self.root.join("imported_agent_context.md")
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MagicToolKind {
    BenchmarkLedger,
    HashEdit,
    ReadResource,
    SearchResource,
    ResolveConflict,
    LspInspect,
    DebugProbe,
    TransactionPreview,
    AstSearch,
    StreamRuleGuard,
    AdvisorWatch,
    SubagentWorkspace,
    EvalKernel,
    MinimizeOutput,
    FirstRunInherit,
    MagicBenchmark,
}

pub struct MagicToolHandler {
    state: Arc<MagicState>,
    kind: MagicToolKind,
}

impl MagicToolHandler {
    pub fn new(state: Arc<MagicState>, kind: MagicToolKind) -> Self {
        Self { state, kind }
    }
}

pub fn builtin_magic_handlers(
    data_dir: PathBuf,
) -> Vec<(Arc<dyn ToolHandler>, &'static str, &'static str)> {
    let state = Arc::new(MagicState::new(data_dir.join("magic_harness")));
    use MagicToolKind::*;
    [
        (BenchmarkLedger, "ledger"),
        (HashEdit, "edit"),
        (ReadResource, "read"),
        (SearchResource, "search"),
        (ResolveConflict, "conflict"),
        (LspInspect, "lsp"),
        (DebugProbe, "debug"),
        (TransactionPreview, "preview"),
        (AstSearch, "ast"),
        (StreamRuleGuard, "guard"),
        (AdvisorWatch, "advisor"),
        (SubagentWorkspace, "agent"),
        (EvalKernel, "eval"),
        (MinimizeOutput, "min"),
        (FirstRunInherit, "inherit"),
        (MagicBenchmark, "bench"),
    ]
    .into_iter()
    .map(|(kind, icon)| {
        (
            Arc::new(MagicToolHandler::new(state.clone(), kind)) as Arc<dyn ToolHandler>,
            "magic",
            icon,
        )
    })
    .collect()
}

#[async_trait]
impl ToolHandler for MagicToolHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        self.state.ensure_dirs();
        match self.kind {
            MagicToolKind::BenchmarkLedger => Ok(benchmark_ledger()),
            MagicToolKind::HashEdit => hash_edit(params).await,
            MagicToolKind::ReadResource => read_resource(params, &self.state).await,
            MagicToolKind::SearchResource => search_resource(params, &self.state).await,
            MagicToolKind::ResolveConflict => resolve_conflict(params).await,
            MagicToolKind::LspInspect => lsp_inspect(params).await,
            MagicToolKind::DebugProbe => debug_probe(params).await,
            MagicToolKind::TransactionPreview => transaction_preview(params, &self.state).await,
            MagicToolKind::AstSearch => ast_search(params).await,
            MagicToolKind::StreamRuleGuard => stream_rule_guard(params, &self.state).await,
            MagicToolKind::AdvisorWatch => advisor_watch(params, &self.state).await,
            MagicToolKind::SubagentWorkspace => subagent_workspace(params, &self.state).await,
            MagicToolKind::EvalKernel => eval_kernel(params, &self.state).await,
            MagicToolKind::MinimizeOutput => Ok(minimize_output_tool(params)),
            MagicToolKind::FirstRunInherit => first_run_inherit(params, &self.state).await,
            MagicToolKind::MagicBenchmark => magic_benchmark(&self.state).await,
        }
    }

    fn schema(&self) -> ToolSchema {
        match self.kind {
            MagicToolKind::BenchmarkLedger => simple_schema(
                "magic_benchmark_ledger",
                "Return the implemented magic-harness gap ledger for all 15 competitive surfaces.",
                vec![],
                vec![],
            ),
            MagicToolKind::HashEdit => simple_schema(
                "hash_edit",
                "Apply a SHA-256 content-hash anchored file edit with stale-anchor detection.",
                vec![
                    str_prop("path", "File to edit."),
                    str_prop("expected_hash", "Optional full or prefix SHA-256 expected for current content."),
                    str_prop("old_string", "Text anchor to replace."),
                    str_prop("new_string", "Replacement text."),
                    int_prop("start_line", "Optional 1-indexed start line."),
                    int_prop("end_line", "Optional inclusive end line."),
                    bool_prop("replace_all", "Replace all exact matches."),
                    bool_prop("dry_run", "Preview without writing."),
                ],
                vec!["path", "new_string"],
            ),
            MagicToolKind::ReadResource => simple_schema(
                "read_resource",
                "Read file://, http(s)://, pr://, issue://, skill://, session://, memory://, agent://, or conflict:// resources.",
                vec![
                    str_prop("uri", "Resource URI or path."),
                    int_prop("offset", "1-indexed line offset."),
                    int_prop("limit", "Line limit."),
                    int_prop("max_chars", "Character cap."),
                ],
                vec!["uri"],
            ),
            MagicToolKind::SearchResource => simple_schema(
                "search_resource",
                "Search a resource URI namespace with regex.",
                vec![str_prop("uri", "Resource URI or path."), str_prop("pattern", "Regex pattern."), int_prop("limit", "Result limit.")],
                vec!["uri", "pattern"],
            ),
            MagicToolKind::ResolveConflict => enum_schema(
                "resolve_conflict",
                "List, read, and resolve git conflict hunks.",
                "action",
                &["list", "read", "resolve"],
                vec![str_prop("path", "File or directory."), int_prop("index", "Conflict index."), str_prop("choice", "ours, theirs, base, or manual."), str_prop("content", "Manual content."), bool_prop("dry_run", "Preview only.")],
            ),
            MagicToolKind::LspInspect => enum_schema(
                "lsp_inspect",
                "Lightweight Rust-native LSP surface: diagnostics, symbols, references, workspace symbols, rename, and code actions.",
                "action",
                &["diagnostics", "symbols", "references", "workspace_symbols", "rename_preview", "rename_apply", "code_action_preview"],
                vec![str_prop("path", "File/workspace path."), str_prop("symbol", "Symbol."), str_prop("replacement", "Replacement symbol."), int_prop("limit", "Limit.")],
            ),
            MagicToolKind::DebugProbe => enum_schema(
                "debug_probe",
                "DAP/debug surface: adapter probes, initialize packet, optional connect_initialize, launch and breakpoint plans.",
                "action",
                &["probe_adapters", "initialize_packet", "connect_initialize", "launch_plan", "breakpoint_plan"],
                vec![str_prop("adapter", "Adapter family."), str_prop("program", "Program path."), str_prop("host", "DAP host."), int_prop("port", "DAP port.")],
            ),
            MagicToolKind::TransactionPreview => enum_schema(
                "transaction_preview",
                "Create, list, read, accept, or reject durable preview cards for risky edits.",
                "action",
                &["create", "list", "read", "accept", "reject"],
                vec![str_prop("id", "Transaction id."), str_prop("kind", "file_write or content_replace, with other kinds preview-only."), obj_prop("payload", "Kind-specific payload.")],
            ),
            MagicToolKind::AstSearch => enum_schema(
                "ast_search",
                "Structural source symbol search and guarded replacement over Rust, Python, JS/TS, and Go.",
                "action",
                &["search", "replace"],
                vec![str_prop("path", "Root/file path."), str_prop("pattern", "Regex over symbol names."), str_prop("symbol_kind", "Optional kind filter."), str_prop("replacement", "Replacement for action=replace."), bool_prop("dry_run", "Preview only."), int_prop("limit", "Limit.")],
            ),
            MagicToolKind::StreamRuleGuard => enum_schema(
                "stream_rule_guard",
                "Persist and evaluate stream guard rules that warn, inject, retry, or abort.",
                "action",
                &["add", "list", "remove", "evaluate"],
                vec![str_prop("id", "Rule id."), str_prop("pattern", "Regex pattern."), str_prop("effect", "warn, inject, retry, or abort."), str_prop("message", "Rule message."), str_prop("text", "Text to evaluate.")],
            ),
            MagicToolKind::AdvisorWatch => simple_schema(
                "advisor_watch",
                "Deterministic advisor pass for blocker/risk/verification findings before finalization.",
                vec![str_prop("transcript", "Draft transcript."), str_prop("objective", "Objective text."), str_prop("evidence", "Evidence text.")],
                vec!["transcript"],
            ),
            MagicToolKind::SubagentWorkspace => enum_schema(
                "subagent_workspace",
                "Create and manage isolated subagent workspaces and agent:// artifacts.",
                "action",
                &["create", "list", "read", "write", "remove"],
                vec![str_prop("id", "Subagent id."), str_prop("goal", "Subagent goal."), str_prop("path", "Artifact path."), str_prop("content", "Artifact content."), bool_prop("git_worktree", "Create a real git worktree."), str_prop("branch", "Worktree branch.")],
            ),
            MagicToolKind::EvalKernel => enum_schema(
                "eval_kernel",
                "Persistent Rust-managed JavaScript/TypeScript/shell eval kernel with Hermes file read/search helpers. Python is unsupported.",
                "action",
                &["run", "read", "reset"],
                vec![str_prop("session", "Kernel session."), str_prop("language", "javascript, typescript, bash, or sh."), str_prop("code", "Code."), int_prop("timeout", "Timeout seconds.")],
            ),
            MagicToolKind::MinimizeOutput => simple_schema(
                "minimize_output",
                "Compress command output into errors, warnings, failures, changed files, and tail context.",
                vec![str_prop("tool", "Command family."), str_prop("output", "Raw output."), int_prop("max_lines", "Tail lines."), int_prop("max_chars", "Character cap.")],
                vec!["output"],
            ),
            MagicToolKind::FirstRunInherit => enum_schema(
                "first_run_inherit",
                "Scan/import rules from Codex, Claude, Cursor, Windsurf, Gemini, Cline, Copilot, and VS Code surfaces.",
                "action",
                &["scan", "import"],
                vec![str_prop("path", "Workspace path."), int_prop("max_chars_per_file", "Excerpt cap.")],
            ),
            MagicToolKind::MagicBenchmark => simple_schema(
                "magic_benchmark",
                "Run deterministic local magic-harness smoke benchmarks.",
                vec![],
                vec![],
            ),
        }
    }
}

fn str_prop(name: &str, description: &str) -> (String, Value) {
    (
        name.into(),
        json!({"type":"string", "description":description}),
    )
}

fn int_prop(name: &str, description: &str) -> (String, Value) {
    (
        name.into(),
        json!({"type":"integer", "description":description}),
    )
}

fn bool_prop(name: &str, description: &str) -> (String, Value) {
    (
        name.into(),
        json!({"type":"boolean", "description":description}),
    )
}

fn obj_prop(name: &str, description: &str) -> (String, Value) {
    (
        name.into(),
        json!({"type":"object", "description":description}),
    )
}

fn simple_schema(
    name: &str,
    description: &str,
    props: Vec<(String, Value)>,
    required: Vec<&str>,
) -> ToolSchema {
    let mut map = IndexMap::new();
    for (key, value) in props {
        map.insert(key, value);
    }
    tool_schema(
        name,
        description,
        JsonSchema::object(map, required.into_iter().map(str::to_string).collect()),
    )
}

fn enum_schema(
    name: &str,
    description: &str,
    action_name: &str,
    actions: &[&str],
    mut props: Vec<(String, Value)>,
) -> ToolSchema {
    props.insert(
        0,
        (
            action_name.into(),
            json!({"type":"string", "enum": actions, "description":"Tool action."}),
        ),
    );
    simple_schema(name, description, props, vec![action_name])
}

fn benchmark_ledger() -> String {
    let rows = [
        (
            1,
            "Magic benchmark ledger",
            "magic_benchmark_ledger + docs ledger",
            "implemented",
        ),
        (2, "Hash-anchored edit engine", "hash_edit", "implemented"),
        (
            3,
            "Unified resource URI layer",
            "read_resource + search_resource",
            "implemented",
        ),
        (
            4,
            "Conflict resolver",
            "resolve_conflict + conflict://",
            "implemented",
        ),
        (
            5,
            "First-class LSP surface",
            "lsp_inspect",
            "implemented_lightweight",
        ),
        (
            6,
            "DAP/debug surface",
            "debug_probe",
            "implemented_probe_packet_connect",
        ),
        (
            7,
            "Preview/accept transaction queue",
            "transaction_preview",
            "implemented",
        ),
        (
            8,
            "Structural AST search/edit",
            "ast_search",
            "implemented_lightweight",
        ),
        (
            9,
            "Mid-stream rule injection",
            "stream_rule_guard",
            "implemented",
        ),
        (10, "Advisor watcher", "advisor_watch", "implemented"),
        (
            11,
            "Subagent worktree fanout",
            "subagent_workspace",
            "implemented",
        ),
        (
            12,
            "Persistent eval kernel",
            "eval_kernel",
            "implemented_js_shell_no_python",
        ),
        (
            13,
            "Tool output minimizer",
            "minimize_output",
            "implemented",
        ),
        (
            14,
            "First-run inheritance",
            "first_run_inherit",
            "implemented",
        ),
        (
            15,
            "Public magic proof",
            "magic_benchmark + docs/magic-benchmarks.md",
            "implemented",
        ),
    ];
    json!({
        "version": MAGIC_VERSION,
        "rust_only_core": true,
        "items": rows.into_iter().map(|(id,item,surface,status)| json!({"id":id,"item":item,"surface":surface,"status":status})).collect::<Vec<_>>()
    })
    .to_string()
}

async fn hash_edit(params: Value) -> Result<String, ToolError> {
    let path = clean_path(PathBuf::from(required_str(&params, "path")?));
    let new_string = params
        .get("new_string")
        .and_then(Value::as_str)
        .unwrap_or("");
    if content_looks_like_internal_read_status(new_string) {
        return Err(ToolError::ExecutionFailed(
            "hash_edit denied: replacement appears to be internal status text".into(),
        ));
    }
    let content = fs::read_to_string(&path).map_err(io_err("read hash_edit file"))?;
    let old_hash = sha256_hex(&content);
    if let Some(expected) = params.get("expected_hash").and_then(Value::as_str) {
        if !expected.trim().is_empty() && !old_hash.starts_with(expected.trim()) {
            return Ok(json!({"applied":false,"reason":"stale_hash","path":path,"expected_hash":expected,"current_hash":old_hash}).to_string());
        }
    }
    let edit = if let Some(old) = params.get("old_string").and_then(Value::as_str) {
        replace_anchor(
            &content,
            old,
            new_string,
            bool_param(&params, "replace_all", false),
        )?
    } else {
        replace_line_range(&content, &params, new_string)?
    };
    let new_hash = sha256_hex(&edit.content);
    let dry_run = bool_param(&params, "dry_run", false);
    if !dry_run {
        CredentialGuard::new().check_write_access(&path, &edit.content)?;
        fs::write(&path, &edit.content).map_err(io_err("write hash_edit file"))?;
    }
    Ok(json!({
        "applied": !dry_run,
        "dry_run": dry_run,
        "path": path,
        "old_hash": old_hash,
        "new_hash": new_hash,
        "match_strategy": edit.strategy,
        "replacements": edit.replacements,
        "bytes_delta": edit.content.len() as i64 - content.len() as i64
    })
    .to_string())
}

struct EditResult {
    content: String,
    strategy: &'static str,
    replacements: usize,
}

fn replace_anchor(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<EditResult, ToolError> {
    if old_string.is_empty() {
        return Err(ToolError::InvalidParams(
            "old_string cannot be empty".into(),
        ));
    }
    let count = content.matches(old_string).count();
    if count > 0 {
        if count > 1 && !replace_all {
            return Err(ToolError::ExecutionFailed(format!(
                "old_string matched {count} times; pass replace_all=true or narrow the anchor"
            )));
        }
        return Ok(EditResult {
            content: if replace_all {
                content.replace(old_string, new_string)
            } else {
                content.replacen(old_string, new_string, 1)
            },
            strategy: "exact",
            replacements: if replace_all { count } else { 1 },
        });
    }
    if let Some((start, end, strategy)) = find_recovered_anchor(content, old_string) {
        let mut out = String::new();
        out.push_str(&content[..start]);
        out.push_str(new_string);
        out.push_str(&content[end..]);
        return Ok(EditResult {
            content: out,
            strategy,
            replacements: 1,
        });
    }
    Err(ToolError::ExecutionFailed(
        "old_string did not match exact, line-trimmed, or whitespace-normalized strategies".into(),
    ))
}

fn find_recovered_anchor(content: &str, old_string: &str) -> Option<(usize, usize, &'static str)> {
    let needle = old_string.lines().map(str::trim).collect::<Vec<_>>();
    let lines = content.lines().collect::<Vec<_>>();
    if !needle.is_empty() {
        for start in 0..lines.len() {
            if start + needle.len() <= lines.len()
                && needle
                    .iter()
                    .enumerate()
                    .all(|(idx, expected)| lines[start + idx].trim() == *expected)
            {
                let (a, b) = byte_range_for_lines(content, start + 1, start + needle.len());
                return Some((a, b, "line_trimmed"));
            }
        }
    }
    let normalized = normalize_ws(old_string);
    let needle_line_count = old_string.lines().count().max(1);
    for start in 0..lines.len() {
        for end in (start + 1)..=lines.len().min(start + needle_line_count + 3) {
            if normalize_ws(&lines[start..end].join("\n")) == normalized {
                let (a, b) = byte_range_for_lines(content, start + 1, end);
                return Some((a, b, "whitespace_normalized"));
            }
        }
    }
    None
}

fn replace_line_range(
    content: &str,
    params: &Value,
    new_string: &str,
) -> Result<EditResult, ToolError> {
    let start = params
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let end = params
        .get("end_line")
        .and_then(Value::as_u64)
        .unwrap_or(start as u64) as usize;
    let lines = content.lines().count().max(1);
    if start > end || end > lines {
        return Err(ToolError::InvalidParams(format!(
            "invalid line range {start}..{end} for {lines} lines"
        )));
    }
    let (a, b) = byte_range_for_lines(content, start, end);
    let mut out = String::new();
    out.push_str(&content[..a]);
    out.push_str(new_string);
    if !new_string.ends_with('\n') && b < content.len() {
        out.push('\n');
    }
    out.push_str(&content[b..]);
    Ok(EditResult {
        content: out,
        strategy: "line_range",
        replacements: 1,
    })
}

async fn read_resource(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let uri = required_str(&params, "uri")?;
    let max_chars = params
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(MAX_RESOURCE_CHARS)
        .clamp(1, MAX_RESOURCE_CHARS);
    let content = resolve_resource(uri, state).await?;
    let content = paginate_lines(
        &content,
        params
            .get("offset")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
        params
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
    );
    Ok(truncate_chars(&content, max_chars))
}

async fn search_resource(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let uri = required_str(&params, "uri")?;
    let pattern = required_str(&params, "pattern")?;
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(50)
        .clamp(1, MAX_SEARCH_RESULTS);
    let re = Regex::new(pattern).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    if let Some(path) = uri_to_local_path(uri, state) {
        if path.is_dir() {
            let mut matches = Vec::new();
            for file in collect_text_files(&path, MAX_SCAN_FILES) {
                let Ok(content) = fs::read_to_string(&file) else {
                    continue;
                };
                for (idx, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        matches.push(json!({"path":file,"line":idx+1,"snippet":truncate_chars(line.trim(),240)}));
                        if matches.len() >= limit {
                            return Ok(json!({"matches":matches,"truncated":true}).to_string());
                        }
                    }
                }
            }
            return Ok(json!({"matches":matches,"truncated":false}).to_string());
        }
    }
    let content = resolve_resource(uri, state).await?;
    let mut matches = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if re.is_match(line) {
            matches.push(json!({"line":idx+1,"snippet":truncate_chars(line.trim(),240)}));
            if matches.len() >= limit {
                return Ok(json!({"matches":matches,"truncated":true}).to_string());
            }
        }
    }
    Ok(json!({"matches":matches,"truncated":false}).to_string())
}

async fn resolve_resource(uri: &str, state: &MagicState) -> Result<String, ToolError> {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return reqwest::get(uri)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("fetch failed: {e}")))?
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("read body failed: {e}")));
    }
    if let Some(path) = uri.strip_prefix("file://") {
        return fs::read_to_string(clean_path(PathBuf::from(path)))
            .map_err(io_err("read file resource"));
    }
    if let Some(spec) = uri.strip_prefix("pr://") {
        return github_resource("pulls", spec).await;
    }
    if let Some(spec) = uri.strip_prefix("issue://") {
        return github_resource("issues", spec).await;
    }
    if let Some(name) = uri.strip_prefix("skill://") {
        return read_skill_resource(name);
    }
    if let Some(id) = uri.strip_prefix("session://") {
        return read_session_resource(id);
    }
    if let Some(query) = uri.strip_prefix("memory://") {
        return read_memory_resource(query);
    }
    if let Some(spec) = uri.strip_prefix("agent://") {
        return read_agent_resource(spec, state);
    }
    if let Some(spec) = uri.strip_prefix("conflict://") {
        return read_conflict_resource(spec);
    }
    fs::read_to_string(clean_path(PathBuf::from(uri))).map_err(io_err("read path resource"))
}

fn uri_to_local_path(uri: &str, state: &MagicState) -> Option<PathBuf> {
    if let Some(path) = uri.strip_prefix("file://") {
        return Some(clean_path(PathBuf::from(path)));
    }
    if let Some(spec) = uri.strip_prefix("agent://") {
        let (id, rel) = split_once_or_all(spec, '/');
        return Some(clean_join(&state.agents_dir().join(safe_id(id)), rel));
    }
    (!uri.contains("://")).then(|| clean_path(PathBuf::from(uri)))
}

async fn github_resource(kind: &str, spec: &str) -> Result<String, ToolError> {
    let (owner, repo, number) = parse_github_spec(spec)?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}/{kind}/{number}");
    let mut req = reqwest::Client::new()
        .get(url)
        .header("User-Agent", "hermes-agent-ultra");
    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        if !token.trim().is_empty() {
            req = req.bearer_auth(token);
        }
    }
    let resp = req
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("GitHub request failed: {e}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("GitHub body failed: {e}")))?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(ToolError::ExecutionFailed(format!(
            "GitHub returned {status}: {}",
            truncate_chars(&body, 500)
        )))
    }
}

fn parse_github_spec(spec: &str) -> Result<(String, String, String), ToolError> {
    let parts = spec
        .split('/')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [number] => {
            let (owner, repo) = github_repo_from_remote()?;
            Ok((owner, repo, (*number).into()))
        }
        [owner, repo, number] => Ok(((*owner).into(), (*repo).into(), (*number).into())),
        _ => Err(ToolError::InvalidParams(
            "GitHub URI must be pr://number or pr://owner/repo/number".into(),
        )),
    }
}

fn github_repo_from_remote() -> Result<(String, String), ToolError> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .map_err(|e| ToolError::ExecutionFailed(format!("git remote lookup failed: {e}")))?;
    let raw = String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_end_matches(".git")
        .to_string();
    if let Some(rest) = raw.strip_prefix("git@github.com:") {
        if let Some((owner, repo)) = rest.split_once('/') {
            return Ok((owner.into(), repo.into()));
        }
    }
    if let Some(rest) = raw.strip_prefix("https://github.com/") {
        if let Some((owner, repo)) = rest.split_once('/') {
            return Ok((owner.into(), repo.into()));
        }
    }
    Err(ToolError::ExecutionFailed(format!(
        "cannot infer GitHub repo from remote: {raw}"
    )))
}

fn read_skill_resource(name: &str) -> Result<String, ToolError> {
    for root in skill_roots() {
        let direct = root.join(name).join("SKILL.md");
        if direct.is_file() {
            return fs::read_to_string(direct).map_err(io_err("read skill"));
        }
        if let Some(found) = find_named_skill(&root, name) {
            return fs::read_to_string(found).map_err(io_err("read skill"));
        }
    }
    Err(ToolError::NotFound(format!("skill not found: {name}")))
}

fn skill_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("skills")];
    if let Some(home) = home_dir() {
        roots.push(home.join(".codex/skills"));
        roots.push(home.join(".agents/skills"));
    }
    roots
}

fn find_named_skill(root: &Path, name: &str) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|s| s.to_str()) == Some(name) {
                    let skill = path.join("SKILL.md");
                    if skill.is_file() {
                        return Some(skill);
                    }
                }
                stack.push(path);
            }
        }
    }
    None
}

fn read_session_resource(id: &str) -> Result<String, ToolError> {
    let root = home_dir()
        .ok_or_else(|| ToolError::NotFound("HOME not set".into()))?
        .join(".codex/sessions");
    let mut matches = Vec::new();
    collect_matching_files(&root, id, &mut matches, 20);
    let Some(path) = matches.first() else {
        return Err(ToolError::NotFound(format!("session not found: {id}")));
    };
    fs::read_to_string(path).map_err(io_err("read session"))
}

fn read_memory_resource(query: &str) -> Result<String, ToolError> {
    let path = home_dir()
        .ok_or_else(|| ToolError::NotFound("HOME not set".into()))?
        .join(".codex/memories/MEMORY.md");
    let content = fs::read_to_string(path).map_err(io_err("read memory"))?;
    let terms = query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let mut out = String::new();
    for (idx, line) in content.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        if terms.iter().any(|term| lower.contains(term)) {
            out.push_str(&format!("{}:{}\n", idx + 1, line));
        }
    }
    Ok(if out.is_empty() {
        "No MEMORY.md lines matched.".into()
    } else {
        out
    })
}

fn read_agent_resource(spec: &str, state: &MagicState) -> Result<String, ToolError> {
    let (id, rel) = split_once_or_all(spec, '/');
    fs::read_to_string(clean_join(&state.agents_dir().join(safe_id(id)), rel))
        .map_err(io_err("read agent resource"))
}

fn read_conflict_resource(spec: &str) -> Result<String, ToolError> {
    let idx = spec.parse::<usize>().unwrap_or(0);
    let conflicts = find_conflicts(Path::new("."))?;
    conflicts
        .get(idx)
        .map(|c| json!(c).to_string())
        .ok_or_else(|| ToolError::NotFound(format!("conflict://{idx} not found")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConflictHunk {
    uri: String,
    path: PathBuf,
    index: usize,
    start_line: usize,
    end_line: usize,
    ours: String,
    base: Option<String>,
    theirs: String,
}

async fn resolve_conflict(params: Value) -> Result<String, ToolError> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    let path = clean_path(PathBuf::from(
        params.get("path").and_then(Value::as_str).unwrap_or("."),
    ));
    match action {
        "list" => Ok(json!({"conflicts": find_conflicts(&path)?}).to_string()),
        "read" => {
            let idx = params.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let conflicts = find_conflicts(&path)?;
            conflicts
                .get(idx)
                .map(|c| json!(c).to_string())
                .ok_or_else(|| ToolError::NotFound(format!("conflict index {idx} not found")))
        }
        "resolve" => resolve_conflict_hunk(&path, &params),
        other => Err(ToolError::InvalidParams(format!(
            "unknown conflict action: {other}"
        ))),
    }
}

fn find_conflicts(path: &Path) -> Result<Vec<ConflictHunk>, ToolError> {
    let files = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        collect_text_files(path, MAX_SCAN_FILES)
    };
    let mut out = Vec::new();
    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        out.extend(parse_conflicts(&file, &content, out.len()));
    }
    Ok(out)
}

fn parse_conflicts(path: &Path, content: &str, start_index: usize) -> Vec<ConflictHunk> {
    let lines = content.lines().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].starts_with("<<<<<<<") {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        let mut ours = Vec::new();
        let mut base = Vec::new();
        let mut theirs = Vec::new();
        while i < lines.len()
            && !lines[i].starts_with("|||||||")
            && !lines[i].starts_with("=======")
        {
            ours.push(lines[i]);
            i += 1;
        }
        let has_base = i < lines.len() && lines[i].starts_with("|||||||");
        if has_base {
            i += 1;
            while i < lines.len() && !lines[i].starts_with("=======") {
                base.push(lines[i]);
                i += 1;
            }
        }
        if i < lines.len() && lines[i].starts_with("=======") {
            i += 1;
        }
        while i < lines.len() && !lines[i].starts_with(">>>>>>>") {
            theirs.push(lines[i]);
            i += 1;
        }
        if i < lines.len() && lines[i].starts_with(">>>>>>>") {
            let index = start_index + out.len();
            out.push(ConflictHunk {
                uri: format!("conflict://{index}"),
                path: path.to_path_buf(),
                index,
                start_line: start + 1,
                end_line: i + 1,
                ours: ours.join("\n"),
                base: has_base.then(|| base.join("\n")),
                theirs: theirs.join("\n"),
            });
        }
        i += 1;
    }
    out
}

fn resolve_conflict_hunk(path: &Path, params: &Value) -> Result<String, ToolError> {
    let conflicts = find_conflicts(path)?;
    let idx = params
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::InvalidParams("resolve requires index".into()))?
        as usize;
    let conflict = conflicts
        .get(idx)
        .ok_or_else(|| ToolError::NotFound(format!("conflict index {idx} not found")))?;
    let choice = required_str(params, "choice")?;
    let replacement = match choice {
        "ours" => conflict.ours.clone(),
        "theirs" => conflict.theirs.clone(),
        "base" => conflict.base.clone().ok_or_else(|| {
            ToolError::InvalidParams("choice=base but no base marker exists".into())
        })?,
        "manual" => required_str(params, "content")?.to_string(),
        other => {
            return Err(ToolError::InvalidParams(format!(
                "unknown conflict choice: {other}"
            )))
        }
    };
    let dry_run = bool_param(params, "dry_run", false);
    let content = fs::read_to_string(&conflict.path).map_err(io_err("read conflict file"))?;
    let (a, b) = byte_range_for_lines(&content, conflict.start_line, conflict.end_line);
    let mut next = String::new();
    next.push_str(&content[..a]);
    next.push_str(&replacement);
    if !replacement.ends_with('\n') && b < content.len() {
        next.push('\n');
    }
    next.push_str(&content[b..]);
    if !dry_run {
        CredentialGuard::new().check_write_access(&conflict.path, &next)?;
        let backup = conflict.path.with_extension("hermes-conflict-backup");
        let _ = fs::write(backup, &content);
        fs::write(&conflict.path, &next).map_err(io_err("write conflict resolution"))?;
    }
    Ok(json!({"resolved":!dry_run,"dry_run":dry_run,"path":conflict.path,"index":idx,"choice":choice,"old_hash":sha256_hex(&content),"new_hash":sha256_hex(&next)}).to_string())
}

#[derive(Debug, Clone, Serialize)]
struct MagicSymbol {
    name: String,
    kind: String,
    line: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ReferenceHit {
    path: PathBuf,
    line: usize,
    snippet: String,
}

async fn lsp_inspect(params: Value) -> Result<String, ToolError> {
    let action = required_str(&params, "action")?;
    let path = clean_path(PathBuf::from(
        params.get("path").and_then(Value::as_str).unwrap_or("."),
    ));
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(50)
        .max(1);
    match action {
        "diagnostics" => Ok(json!({"diagnostics": diagnostics_for_path(&path)}).to_string()),
        "symbols" => {
            let content = fs::read_to_string(&path).map_err(io_err("read symbols file"))?;
            Ok(json!({"symbols": extract_symbols(&path, &content).into_iter().take(limit).collect::<Vec<_>>()}).to_string())
        }
        "workspace_symbols" => {
            let mut symbols = Vec::new();
            for file in collect_source_files(&path, MAX_SCAN_FILES) {
                let Ok(content) = fs::read_to_string(&file) else {
                    continue;
                };
                for symbol in extract_symbols(&file, &content) {
                    symbols.push(json!({"path":file,"symbol":symbol}));
                    if symbols.len() >= limit {
                        return Ok(json!({"symbols":symbols,"truncated":true}).to_string());
                    }
                }
            }
            Ok(json!({"symbols":symbols,"truncated":false}).to_string())
        }
        "references" => Ok(
            json!({"references": find_references(&path, required_str(&params, "symbol")?, limit)})
                .to_string(),
        ),
        "rename_preview" | "rename_apply" => {
            rename_symbol(&path, &params, action == "rename_apply")
        }
        "code_action_preview" => {
            Ok(json!({"path":path,"actions":code_action_preview(&path)}).to_string())
        }
        other => Err(ToolError::InvalidParams(format!(
            "unknown lsp action: {other}"
        ))),
    }
}

fn diagnostics_for_path(path: &Path) -> Vec<Value> {
    let files = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        collect_source_files(path, MAX_SCAN_FILES)
    };
    let mut out = Vec::new();
    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let mut stack = Vec::new();
        for (idx, ch) in content.chars().enumerate() {
            match ch {
                '{' | '(' | '[' => stack.push((ch, idx)),
                '}' | ')' | ']' if !matches_pair(stack.pop().map(|p| p.0), ch) => {
                    out.push(json!({"path":file,"severity":"error","byte":idx,"message":format!("unmatched closing delimiter {ch}")}));
                }
                _ => {}
            }
        }
        for (ch, idx) in stack.into_iter().take(5) {
            out.push(json!({"path":file,"severity":"error","byte":idx,"message":format!("unclosed delimiter {ch}")}));
        }
    }
    out
}

fn matches_pair(open: Option<char>, close: char) -> bool {
    matches!(
        (open, close),
        (Some('{'), '}') | (Some('('), ')') | (Some('['), ']')
    )
}

fn extract_symbols(path: &Path, content: &str) -> Vec<MagicSymbol> {
    let patterns: &[(&str, &str)] = match language_for_path(path) {
        "rust" => &[
            (
                "function",
                r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)",
            ),
            (
                "struct",
                r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)",
            ),
            (
                "enum",
                r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)",
            ),
            (
                "trait",
                r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)",
            ),
            (
                "const",
                r"^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+([A-Za-z_][A-Za-z0-9_]*)",
            ),
        ],
        "python" => &[
            ("function", r"^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\("),
            ("class", r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)"),
        ],
        "javascript" | "typescript" => &[
            (
                "function",
                r"^\s*(?:export\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
            ),
            (
                "class",
                r"^\s*(?:export\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)",
            ),
            (
                "interface",
                r"^\s*(?:export\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)",
            ),
            (
                "type",
                r"^\s*(?:export\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)\s*=",
            ),
            (
                "const",
                r"^\s*(?:export\s+)?const\s+([A-Za-z_][A-Za-z0-9_]*)\s*=",
            ),
        ],
        "go" => &[
            (
                "function",
                r"^\s*func\s+(?:\([^)]+\)\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*\(",
            ),
            ("type", r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+"),
        ],
        _ => &[],
    };
    let compiled = patterns
        .iter()
        .filter_map(|(kind, pat)| Regex::new(pat).ok().map(|re| (*kind, re)))
        .collect::<Vec<_>>();
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        for (kind, re) in &compiled {
            if let Some(name) = re.captures(line).and_then(|caps| caps.get(1)) {
                out.push(MagicSymbol {
                    name: name.as_str().into(),
                    kind: (*kind).into(),
                    line: idx + 1,
                });
            }
        }
    }
    out
}

fn find_references(root_or_file: &Path, symbol: &str, limit: usize) -> Vec<ReferenceHit> {
    if !looks_like_identifier(symbol) {
        return Vec::new();
    }
    let Ok(re) = Regex::new(&format!(r"\b{}\b", regex::escape(symbol))) else {
        return Vec::new();
    };
    let files = if root_or_file.is_file() {
        vec![root_or_file.to_path_buf()]
    } else {
        collect_source_files(root_or_file, MAX_SCAN_FILES)
    };
    let mut out = Vec::new();
    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if re.is_match(line) {
                out.push(ReferenceHit {
                    path: file.clone(),
                    line: idx + 1,
                    snippet: truncate_chars(line.trim(), 240),
                });
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

fn rename_symbol(path: &Path, params: &Value, apply: bool) -> Result<String, ToolError> {
    let symbol = required_str(params, "symbol")?;
    let replacement = required_str(params, "replacement")?;
    if !looks_like_identifier(symbol) || !looks_like_identifier(replacement) {
        return Err(ToolError::InvalidParams(
            "symbol and replacement must be identifier-like".into(),
        ));
    }
    let re = Regex::new(&format!(r"\b{}\b", regex::escape(symbol)))
        .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    let files = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        collect_source_files(path, MAX_SCAN_FILES)
    };
    let mut touched = Vec::new();
    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        if !re.is_match(&content) {
            continue;
        }
        let count = re.find_iter(&content).count();
        let next = re.replace_all(&content, replacement).to_string();
        if apply {
            CredentialGuard::new().check_write_access(&file, &next)?;
            fs::write(&file, &next).map_err(io_err("write rename"))?;
        }
        touched.push(json!({"path":file,"replacements":count,"old_hash":sha256_hex(&content),"new_hash":sha256_hex(&next)}));
    }
    Ok(
        json!({"applied":apply,"symbol":symbol,"replacement":replacement,"files":touched})
            .to_string(),
    )
}

fn code_action_preview(path: &Path) -> Vec<Value> {
    let mut actions = Vec::new();
    let diagnostics = diagnostics_for_path(path);
    if !diagnostics.is_empty() {
        actions.push(json!({"kind":"quickfix.diagnostics","title":"Fix unmatched delimiters then run formatter/compiler","diagnostic_count":diagnostics.len()}));
    }
    let files = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        collect_source_files(path, 60)
    };
    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        if content.contains("TODO") || content.contains("todo!") {
            actions.push(json!({"kind":"refactor.todo","title":"Resolve TODO/todo! before claiming complete","path":file}));
        }
    }
    actions
}

async fn debug_probe(params: Value) -> Result<String, ToolError> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("probe_adapters");
    match action {
        "probe_adapters" => Ok(json!({"adapters": probe_debug_adapters().await}).to_string()),
        "initialize_packet" => Ok(json!({"packet": dap_initialize_packet()}).to_string()),
        "connect_initialize" => connect_dap_initialize(&params).await,
        "launch_plan" => Ok(json!({
            "adapter": params.get("adapter").and_then(Value::as_str).unwrap_or("lldb"),
            "program": params.get("program").and_then(Value::as_str).unwrap_or(""),
            "request": "launch",
            "sequence": ["initialize", "launch", "setBreakpoints", "configurationDone", "threads", "stackTrace", "scopes", "variables"],
            "external_adapter_required": true,
            "rust_only_core": true
        }).to_string()),
        "breakpoint_plan" => Ok(json!({"program":params.get("program").and_then(Value::as_str).unwrap_or(""),"request":"setBreakpoints","source_required":true,"lines_required":true}).to_string()),
        other => Err(ToolError::InvalidParams(format!("unknown debug action: {other}"))),
    }
}

async fn probe_debug_adapters() -> Vec<Value> {
    let mut out = Vec::new();
    for (adapter, bin) in [
        ("lldb", "lldb"),
        ("codelldb", "codelldb"),
        ("dlv", "dlv"),
        ("node", "node"),
        ("debugpy", "python3"),
    ] {
        out.push(json!({"adapter":adapter,"binary":bin,"available":command_available(bin).await}));
    }
    out
}

async fn command_available(bin: &str) -> bool {
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} >/dev/null 2>&1", shell_quote(bin)))
        .output()
        .await
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn dap_initialize_packet() -> String {
    let body = json!({
        "seq":1,
        "type":"request",
        "command":"initialize",
        "arguments":{"adapterID":"hermes-debug-probe","clientID":"hermes-agent-ultra","clientName":"Hermes Agent Ultra","linesStartAt1":true,"columnsStartAt1":true,"supportsVariableType":true,"supportsRunInTerminalRequest":false}
    }).to_string();
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

async fn connect_dap_initialize(params: &Value) -> Result<String, ToolError> {
    let host = params
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("127.0.0.1");
    let port = params
        .get("port")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::InvalidParams("connect_initialize requires port".into()))?;
    let addr = format!("{host}:{port}");
    let packet = dap_initialize_packet();
    let fut = async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("DAP connect failed: {e}")))?;
        stream
            .write_all(packet.as_bytes())
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("DAP write failed: {e}")))?;
        let mut buf = vec![0; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("DAP read failed: {e}")))?;
        Ok::<_, ToolError>(String::from_utf8_lossy(&buf[..n]).to_string())
    };
    let response = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .map_err(|_| ToolError::Timeout("DAP initialize timed out".into()))??;
    Ok(json!({"connected":true,"response":response}).to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreviewTransaction {
    id: String,
    kind: String,
    status: String,
    created_at: String,
    updated_at: String,
    payload: Value,
}

async fn transaction_preview(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let action = required_str(&params, "action")?;
    match action {
        "create" => {
            let kind = required_str(&params, "kind")?.to_string();
            let id = format!("txn-{}", uuid::Uuid::new_v4());
            let now = Utc::now().to_rfc3339();
            let txn = PreviewTransaction {
                id: id.clone(),
                kind,
                status: "pending".into(),
                created_at: now.clone(),
                updated_at: now,
                payload: params.get("payload").cloned().unwrap_or_else(|| json!({})),
            };
            write_transaction(state, &txn)?;
            Ok(json!(txn).to_string())
        }
        "list" => {
            let mut txns = Vec::new();
            for entry in fs::read_dir(state.transactions_dir())
                .map_err(io_err("list transactions"))?
                .flatten()
            {
                if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(txn) = read_transaction_path(&entry.path()) {
                        txns.push(txn);
                    }
                }
            }
            txns.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(json!({"transactions":txns}).to_string())
        }
        "read" => Ok(json!(read_transaction(state, required_str(&params, "id")?)?).to_string()),
        "accept" | "reject" => {
            let mut txn = read_transaction(state, required_str(&params, "id")?)?;
            if action == "accept" {
                apply_transaction(&txn)?;
                txn.status = "accepted".into();
            } else {
                txn.status = "rejected".into();
            }
            txn.updated_at = Utc::now().to_rfc3339();
            write_transaction(state, &txn)?;
            Ok(json!(txn).to_string())
        }
        other => Err(ToolError::InvalidParams(format!(
            "unknown transaction action: {other}"
        ))),
    }
}

fn transaction_path(state: &MagicState, id: &str) -> PathBuf {
    state
        .transactions_dir()
        .join(format!("{}.json", safe_id(id)))
}

fn write_transaction(state: &MagicState, txn: &PreviewTransaction) -> Result<(), ToolError> {
    fs::write(
        transaction_path(state, &txn.id),
        serde_json::to_vec_pretty(txn).map_err(to_tool_err)?,
    )
    .map_err(io_err("write transaction"))
}

fn read_transaction(state: &MagicState, id: &str) -> Result<PreviewTransaction, ToolError> {
    read_transaction_path(&transaction_path(state, id))
}

fn read_transaction_path(path: &Path) -> Result<PreviewTransaction, ToolError> {
    serde_json::from_slice(&fs::read(path).map_err(io_err("read transaction"))?)
        .map_err(to_tool_err)
}

fn apply_transaction(txn: &PreviewTransaction) -> Result<(), ToolError> {
    match txn.kind.as_str() {
        "file_write" => {
            let path = clean_path(PathBuf::from(
                txn.payload
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidParams("file_write requires payload.path".into())
                    })?,
            ));
            let content = txn
                .payload
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ToolError::InvalidParams("file_write requires payload.content".into())
                })?;
            CredentialGuard::new().check_write_access(&path, content)?;
            fs::write(path, content).map_err(io_err("transaction file_write"))
        }
        "content_replace" => {
            let path = clean_path(PathBuf::from(
                txn.payload
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ToolError::InvalidParams("content_replace requires payload.path".into())
                    })?,
            ));
            let old = txn
                .payload
                .get("old_string")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ToolError::InvalidParams("content_replace requires payload.old_string".into())
                })?;
            let new = txn
                .payload
                .get("new_string")
                .and_then(Value::as_str)
                .unwrap_or("");
            let content = fs::read_to_string(&path).map_err(io_err("transaction read"))?;
            let edit = replace_anchor(&content, old, new, false)?;
            CredentialGuard::new().check_write_access(&path, &edit.content)?;
            fs::write(path, edit.content).map_err(io_err("transaction content_replace"))
        }
        other => Err(ToolError::ExecutionFailed(format!(
            "transaction kind {other} is preview-only; use the dedicated tool to apply it"
        ))),
    }
}

async fn ast_search(params: Value) -> Result<String, ToolError> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("search");
    let root = clean_path(PathBuf::from(
        params.get("path").and_then(Value::as_str).unwrap_or("."),
    ));
    let pattern = required_str(&params, "pattern")?;
    let kind_filter = params.get("symbol_kind").and_then(Value::as_str);
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(50)
        .max(1);
    let re = Regex::new(pattern).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    let files = if root.is_file() {
        vec![root.clone()]
    } else {
        collect_source_files(&root, MAX_SCAN_FILES)
    };
    let mut matches = Vec::new();
    let mut names = BTreeSet::new();
    for file in &files {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        for sym in extract_symbols(file, &content) {
            if kind_filter.is_some_and(|k| k != sym.kind) {
                continue;
            }
            if re.is_match(&sym.name) {
                names.insert(sym.name.clone());
                matches.push(json!({"path":file,"symbol":sym}));
                if matches.len() >= limit {
                    break;
                }
            }
        }
        if matches.len() >= limit {
            break;
        }
    }
    if action == "search" {
        return Ok(json!({"matches":matches,"truncated":matches.len()>=limit}).to_string());
    }
    if action != "replace" {
        return Err(ToolError::InvalidParams(format!(
            "unknown ast action: {action}"
        )));
    }
    let replacement = required_str(&params, "replacement")?;
    if !looks_like_identifier(replacement) {
        return Err(ToolError::InvalidParams(
            "replacement must be identifier-like".into(),
        ));
    }
    let dry_run = bool_param(&params, "dry_run", true);
    let mut touched = Vec::new();
    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let mut next = content.clone();
        let mut count = 0;
        for name in &names {
            if !looks_like_identifier(name) {
                continue;
            }
            let word = Regex::new(&format!(r"\b{}\b", regex::escape(name)))
                .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
            count += word.find_iter(&next).count();
            next = word.replace_all(&next, replacement).to_string();
        }
        if count > 0 {
            if !dry_run {
                CredentialGuard::new().check_write_access(&file, &next)?;
                fs::write(&file, &next).map_err(io_err("ast replace"))?;
            }
            touched.push(json!({"path":file,"replacements":count,"old_hash":sha256_hex(&content),"new_hash":sha256_hex(&next)}));
        }
    }
    Ok(json!({"applied":!dry_run,"matches":matches,"touched":touched}).to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StreamRule {
    id: String,
    pattern: String,
    effect: String,
    message: String,
}

async fn stream_rule_guard(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("evaluate");
    let mut rules = load_stream_rules(state)?;
    match action {
        "add" => {
            let pattern = required_str(&params, "pattern")?.to_string();
            Regex::new(&pattern).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("rule-{}", uuid::Uuid::new_v4()));
            let effect = params
                .get("effect")
                .and_then(Value::as_str)
                .unwrap_or("warn")
                .to_string();
            let message = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("stream guard matched")
                .to_string();
            rules.retain(|r| r.id != id);
            rules.push(StreamRule {
                id,
                pattern,
                effect,
                message,
            });
            save_stream_rules(state, &rules)?;
            Ok(json!({"rules":rules}).to_string())
        }
        "list" => Ok(json!({"rules":rules}).to_string()),
        "remove" => {
            let id = required_str(&params, "id")?;
            rules.retain(|r| r.id != id);
            save_stream_rules(state, &rules)?;
            Ok(json!({"rules":rules}).to_string())
        }
        "evaluate" => Ok(json!(evaluate_stream_rules(
            params.get("text").and_then(Value::as_str).unwrap_or(""),
            &rules
        ))
        .to_string()),
        other => Err(ToolError::InvalidParams(format!(
            "unknown stream rule action: {other}"
        ))),
    }
}

fn default_stream_rules() -> Vec<StreamRule> {
    vec![
        StreamRule { id: "no-fake-passed-tests".into(), pattern: r"(?i)\b(passed|green|verified)\b".into(), effect: "warn".into(), message: "Only claim tests passed when command evidence is present.".into() },
        StreamRule { id: "no-python-runtime-default".into(), pattern: r"(?i)\bpython\b.*\b(default|required)\b".into(), effect: "inject".into(), message: "Hermes Agent Ultra core runtime is Rust-only; keep Python optional and policy-gated.".into() },
        StreamRule { id: "no-status-only-completion".into(), pattern: r"(?i)\bwill implement\b|\bnext I will\b".into(), effect: "warn".into(), message: "For proceed requests, execute implementation work instead of stopping at intent.".into() },
    ]
}

fn load_stream_rules(state: &MagicState) -> Result<Vec<StreamRule>, ToolError> {
    if !state.rules_path().is_file() {
        return Ok(default_stream_rules());
    }
    serde_json::from_slice(&fs::read(state.rules_path()).map_err(io_err("read stream rules"))?)
        .map_err(to_tool_err)
}

fn save_stream_rules(state: &MagicState, rules: &[StreamRule]) -> Result<(), ToolError> {
    fs::write(
        state.rules_path(),
        serde_json::to_vec_pretty(rules).map_err(to_tool_err)?,
    )
    .map_err(io_err("write stream rules"))
}

fn evaluate_stream_rules(text: &str, rules: &[StreamRule]) -> Value {
    let mut verdict = "allow";
    let mut matches = Vec::new();
    for rule in rules {
        let Ok(re) = Regex::new(&rule.pattern) else {
            continue;
        };
        if re.is_match(text) {
            verdict = match (verdict, rule.effect.as_str()) {
                (_, "abort") => "abort",
                ("abort", _) => "abort",
                (_, "retry") => "retry",
                ("retry", _) => "retry",
                (_, "inject") => "inject",
                ("inject", _) => "inject",
                _ => "warn",
            };
            matches.push(json!(rule));
        }
    }
    json!({"verdict":verdict,"matches":matches})
}

async fn advisor_watch(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let transcript = required_str(&params, "transcript")?;
    let objective = params
        .get("objective")
        .and_then(Value::as_str)
        .unwrap_or("");
    let evidence = params.get("evidence").and_then(Value::as_str).unwrap_or("");
    let lower = transcript.to_ascii_lowercase();
    let evidence_lower = evidence.to_ascii_lowercase();
    let mut findings = Vec::new();
    if contains_verification_claim(&lower)
        && !contains_command_evidence(&lower)
        && !contains_command_evidence(&evidence_lower)
    {
        findings.push(json!({"severity":"blocker","kind":"verification_required","message":"Verification claim lacks command/artifact evidence."}));
    }
    if lower.contains("placeholder") || lower.contains("stub") || lower.contains("todo") {
        findings.push(json!({"severity":"risk","kind":"incomplete_implementation","message":"Draft mentions placeholder/stub/TODO."}));
    }
    if lower.contains("passed") && lower.contains("failed") {
        findings.push(json!({"severity":"risk","kind":"contradictory_evidence","message":"Draft contains both passed and failed claims."}));
    }
    if !objective.trim().is_empty() && !lower.contains(&objective.to_ascii_lowercase()) {
        findings.push(json!({"severity":"note","kind":"objective_drift","message":"Objective text is not reflected in transcript."}));
    }
    let rule_eval = evaluate_stream_rules(transcript, &load_stream_rules(state)?);
    let blocked = findings
        .iter()
        .any(|f| f.get("severity").and_then(Value::as_str) == Some("blocker"))
        || rule_eval.get("verdict").and_then(Value::as_str) == Some("abort");
    Ok(
        json!({"blocked":blocked,"findings":findings,"stream_rule_evaluation":rule_eval})
            .to_string(),
    )
}

fn contains_verification_claim(lower: &str) -> bool {
    ["passed", "verified", "green", "tested", "checks passed"]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn contains_command_evidence(lower: &str) -> bool {
    [
        "cargo ",
        "pytest",
        "npm test",
        "git diff --check",
        "exit code",
        "process exited",
        "passed:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubagentManifest {
    id: String,
    goal: String,
    path: PathBuf,
    branch: Option<String>,
    git_worktree: bool,
    created_at: String,
    status: String,
}

async fn subagent_workspace(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("create");
    match action {
        "create" => {
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .map(safe_id)
                .unwrap_or_else(|| format!("agent-{}", uuid::Uuid::new_v4()));
            let goal = params
                .get("goal")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let git_worktree = bool_param(&params, "git_worktree", false);
            let branch = params
                .get("branch")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| git_worktree.then(|| format!("subagent/{id}")));
            let path = state.agents_dir().join(&id);
            if git_worktree {
                let branch_name = branch.clone().unwrap_or_else(|| format!("subagent/{id}"));
                let status = std::process::Command::new("git")
                    .args(["worktree", "add", "-b", &branch_name])
                    .arg(&path)
                    .arg("HEAD")
                    .status()
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("git worktree add failed: {e}"))
                    })?;
                if !status.success() {
                    return Err(ToolError::ExecutionFailed(format!(
                        "git worktree add exited with {status}"
                    )));
                }
            } else {
                fs::create_dir_all(&path).map_err(io_err("create subagent workspace"))?;
            }
            let manifest = SubagentManifest {
                id: id.clone(),
                goal,
                path: path.clone(),
                branch,
                git_worktree,
                created_at: Utc::now().to_rfc3339(),
                status: "active".into(),
            };
            write_subagent_manifest(&manifest)?;
            Ok(
                json!({"manifest":manifest,"uri":format!("agent://{id}/manifest.json")})
                    .to_string(),
            )
        }
        "list" => {
            let mut agents = Vec::new();
            for entry in fs::read_dir(state.agents_dir())
                .map_err(io_err("list subagents"))?
                .flatten()
            {
                let manifest = entry.path().join("manifest.json");
                if manifest.is_file() {
                    if let Ok(agent) = read_subagent_manifest_path(&manifest) {
                        agents.push(agent);
                    }
                }
            }
            Ok(json!({"agents":agents}).to_string())
        }
        "read" => {
            let id = safe_id(required_str(&params, "id")?);
            let rel = params
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("manifest.json");
            fs::read_to_string(clean_join(&state.agents_dir().join(id), rel))
                .map_err(io_err("read subagent artifact"))
        }
        "write" => {
            let id = safe_id(required_str(&params, "id")?);
            let rel = required_str(&params, "path")?;
            let content = params.get("content").and_then(Value::as_str).unwrap_or("");
            let base = state.agents_dir().join(&id);
            let path = clean_join(&base, rel);
            if !path.starts_with(&base) {
                return Err(ToolError::InvalidParams(
                    "subagent path escaped workspace".into(),
                ));
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(io_err("create subagent artifact dir"))?;
            }
            fs::write(&path, content).map_err(io_err("write subagent artifact"))?;
            Ok(json!({"written":true,"uri":format!("agent://{id}/{rel}")}).to_string())
        }
        "remove" => {
            let id = safe_id(required_str(&params, "id")?);
            let manifest =
                read_subagent_manifest_path(&state.agents_dir().join(&id).join("manifest.json"))?;
            if manifest.git_worktree {
                let status = std::process::Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(&manifest.path)
                    .status()
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("git worktree remove failed: {e}"))
                    })?;
                if !status.success() {
                    return Err(ToolError::ExecutionFailed(format!(
                        "git worktree remove exited with {status}"
                    )));
                }
            } else if manifest.path.exists() {
                fs::remove_dir_all(&manifest.path).map_err(io_err("remove subagent workspace"))?;
            }
            Ok(json!({"removed":true,"id":id}).to_string())
        }
        other => Err(ToolError::InvalidParams(format!(
            "unknown subagent action: {other}"
        ))),
    }
}

fn write_subagent_manifest(manifest: &SubagentManifest) -> Result<(), ToolError> {
    fs::create_dir_all(&manifest.path).map_err(io_err("create subagent dir"))?;
    fs::write(
        manifest.path.join("manifest.json"),
        serde_json::to_vec_pretty(manifest).map_err(to_tool_err)?,
    )
    .map_err(io_err("write subagent manifest"))
}

fn read_subagent_manifest_path(path: &Path) -> Result<SubagentManifest, ToolError> {
    serde_json::from_slice(&fs::read(path).map_err(io_err("read subagent manifest"))?)
        .map_err(to_tool_err)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KernelSession {
    session: String,
    language: String,
    snippets: Vec<String>,
    updated_at: String,
}

async fn eval_kernel(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("run");
    let session = params
        .get("session")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let path = state
        .kernels_dir()
        .join(format!("{}.json", safe_id(session)));
    match action {
        "reset" => {
            let _ = fs::remove_file(path);
            Ok(json!({"reset":true,"session":session}).to_string())
        }
        "read" => Ok(json!(read_kernel(&path).unwrap_or_else(|| KernelSession {
            session: session.into(),
            language: "javascript".into(),
            snippets: Vec::new(),
            updated_at: Utc::now().to_rfc3339()
        }))
        .to_string()),
        "run" => {
            let language = params
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("javascript");
            if language.eq_ignore_ascii_case("python") {
                return Err(ToolError::InvalidParams("Python is unsupported in eval_kernel; use javascript, typescript, bash, or sh.".into()));
            }
            let code = required_str(&params, "code")?;
            let timeout = params
                .get("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 60);
            let mut kernel = read_kernel(&path).unwrap_or_else(|| KernelSession {
                session: session.into(),
                language: language.into(),
                snippets: Vec::new(),
                updated_at: Utc::now().to_rfc3339(),
            });
            if kernel.language != language {
                return Err(ToolError::InvalidParams(format!(
                    "kernel language is {}; reset or use same language",
                    kernel.language
                )));
            }
            let output = run_kernel_code(state, &kernel, code, language, timeout).await?;
            kernel.snippets.push(code.to_string());
            kernel.updated_at = Utc::now().to_rfc3339();
            write_kernel(&path, &kernel)?;
            Ok(json!({"session":session,"language":language,"output":output}).to_string())
        }
        other => Err(ToolError::InvalidParams(format!(
            "unknown eval action: {other}"
        ))),
    }
}

fn read_kernel(path: &Path) -> Option<KernelSession> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_kernel(path: &Path, kernel: &KernelSession) -> Result<(), ToolError> {
    fs::write(
        path,
        serde_json::to_vec_pretty(kernel).map_err(to_tool_err)?,
    )
    .map_err(io_err("write kernel"))
}

async fn run_kernel_code(
    state: &MagicState,
    kernel: &KernelSession,
    code: &str,
    language: &str,
    timeout: u64,
) -> Result<String, ToolError> {
    match language {
        "javascript" | "typescript" => run_js_kernel(state, kernel, code, timeout).await,
        "bash" | "sh" => run_shell_kernel(kernel, code, timeout).await,
        other => Err(ToolError::InvalidParams(format!(
            "unsupported eval language: {other}"
        ))),
    }
}

async fn run_js_kernel(
    state: &MagicState,
    kernel: &KernelSession,
    code: &str,
    timeout: u64,
) -> Result<String, ToolError> {
    if !command_available("node").await {
        return Err(ToolError::ExecutionFailed(
            "node is required for javascript eval_kernel sessions".into(),
        ));
    }
    let script_path = state
        .kernels_dir()
        .join(format!("{}.js", safe_id(&kernel.session)));
    let mut script = String::from(JS_HELPER);
    for snippet in &kernel.snippets {
        script.push_str("\n// previous snippet\n");
        script.push_str(snippet);
        script.push('\n');
    }
    script.push_str("\n// current snippet\n");
    script.push_str(code);
    script.push('\n');
    fs::write(&script_path, script).map_err(io_err("write js kernel script"))?;
    run_command_with_timeout("node", &[script_path.to_string_lossy().as_ref()], timeout).await
}

const JS_HELPER: &str = r#"
const fs = require('fs');
const path = require('path');
globalThis.hermes = {
  read: (target) => fs.readFileSync(target.replace(/^file:\/\//, ''), 'utf8'),
  search: (pattern, target='.') => {
    const re = new RegExp(pattern);
    const out = [];
    const walk = (p) => {
      for (const ent of fs.readdirSync(p, {withFileTypes: true})) {
        const fp = path.join(p, ent.name);
        if (ent.isDirectory()) { if (!['.git','target','node_modules'].includes(ent.name)) walk(fp); continue; }
        if (!ent.isFile()) continue;
        const text = fs.readFileSync(fp, 'utf8');
        text.split(/\r?\n/).forEach((line, idx) => { if (re.test(line)) out.push({path: fp, line: idx + 1, snippet: line.trim()}); });
      }
    };
    walk(target);
    return out;
  }
};
"#;

async fn run_shell_kernel(
    kernel: &KernelSession,
    code: &str,
    timeout: u64,
) -> Result<String, ToolError> {
    let mut script = String::new();
    for snippet in &kernel.snippets {
        script.push_str(snippet);
        script.push('\n');
    }
    script.push_str(code);
    run_command_with_timeout("sh", &["-c", &script], timeout).await
}

async fn run_command_with_timeout(
    cmd: &str,
    args: &[&str],
    timeout_secs: u64,
) -> Result<String, ToolError> {
    let mut command = tokio::process::Command::new(cmd);
    command.args(args);
    let out = tokio::time::timeout(Duration::from_secs(timeout_secs), command.output())
        .await
        .map_err(|_| ToolError::Timeout(format!("{cmd} timed out")))?
        .map_err(|e| ToolError::ExecutionFailed(format!("{cmd} failed: {e}")))?;
    Ok(json!({"exit_code":out.status.code(),"stdout":String::from_utf8_lossy(&out.stdout),"stderr":String::from_utf8_lossy(&out.stderr)}).to_string())
}

#[derive(Debug, Clone, Serialize)]
struct OutputSummary {
    tool: String,
    original_lines: usize,
    original_chars: usize,
    errors: Vec<String>,
    warnings: Vec<String>,
    failures: Vec<String>,
    changed_files: Vec<String>,
    tail: Vec<String>,
    minimized: String,
}

fn minimize_output_tool(params: Value) -> String {
    let tool = params
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("generic");
    let output = params.get("output").and_then(Value::as_str).unwrap_or("");
    let max_lines = params
        .get("max_lines")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(80)
        .max(1);
    let max_chars = params
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(12_000)
        .max(1);
    json!(minimize_output(tool, output, max_lines, max_chars)).to_string()
}

fn minimize_output(tool: &str, output: &str, max_lines: usize, max_chars: usize) -> OutputSummary {
    let lines = output.lines().map(str::to_string).collect::<Vec<_>>();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    let mut changed_files = BTreeSet::new();
    for line in &lines {
        let lower = line.to_ascii_lowercase();
        if lower.contains("error") || lower.contains("panicked") || lower.contains("traceback") {
            push_limited(&mut errors, line);
        }
        if lower.contains("warning") || lower.contains("warn:") {
            push_limited(&mut warnings, line);
        }
        if lower.contains("failed") || lower.contains("failure") || lower.contains("failing") {
            push_limited(&mut failures, line);
        }
        if (matches!(tool, "git" | "gh")
            || lower.starts_with("modified:")
            || lower.starts_with("new file:"))
            && looks_like_path_line(line)
        {
            changed_files.insert(line.trim().to_string());
        }
    }
    let tail = lines
        .iter()
        .rev()
        .take(max_lines.min(lines.len()))
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let mut minimized = String::new();
    render_section(&mut minimized, "errors", &errors);
    render_section(&mut minimized, "warnings", &warnings);
    render_section(&mut minimized, "failures", &failures);
    if !changed_files.is_empty() {
        minimized.push_str("## changed_files\n");
        for file in &changed_files {
            minimized.push_str("- ");
            minimized.push_str(file);
            minimized.push('\n');
        }
    }
    render_section(&mut minimized, "tail", &tail);
    OutputSummary {
        tool: tool.into(),
        original_lines: lines.len(),
        original_chars: output.chars().count(),
        errors,
        warnings,
        failures,
        changed_files: changed_files.into_iter().collect(),
        tail,
        minimized: truncate_chars(&minimized, max_chars),
    }
}

fn push_limited(target: &mut Vec<String>, line: &str) {
    if target.len() < 40 {
        target.push(truncate_chars(line.trim(), 500));
    }
}

fn render_section(out: &mut String, name: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push_str("## ");
    out.push_str(name);
    out.push('\n');
    for line in lines {
        out.push_str("- ");
        out.push_str(line.trim());
        out.push('\n');
    }
}

fn looks_like_path_line(line: &str) -> bool {
    let s = line.trim();
    s.contains('/')
        || [".rs", ".py", ".ts", ".js", ".md", ".toml"]
            .iter()
            .any(|ext| s.contains(ext))
}

#[derive(Debug, Clone, Serialize)]
struct InheritFinding {
    surface: String,
    path: PathBuf,
    bytes: u64,
    excerpt: String,
}

async fn first_run_inherit(params: Value, state: &MagicState) -> Result<String, ToolError> {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("scan");
    let root = clean_path(PathBuf::from(
        params.get("path").and_then(Value::as_str).unwrap_or("."),
    ));
    let max_chars = params
        .get("max_chars_per_file")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(4_000)
        .max(100);
    let findings = scan_agent_configs(&root, max_chars);
    if action == "scan" {
        return Ok(json!({"findings":findings}).to_string());
    }
    if action != "import" {
        return Err(ToolError::InvalidParams(format!(
            "unknown inherit action: {action}"
        )));
    }
    let mut doc = String::from("# Imported Agent Context\n\nGenerated by Hermes first_run_inherit. Review before relying on inherited rules.\n\n");
    for finding in &findings {
        doc.push_str(&format!(
            "## {}\n\n```text\n{}\n```\n\n",
            finding.path.display(),
            finding.excerpt
        ));
    }
    fs::write(state.import_path(), doc).map_err(io_err("write imported context"))?;
    Ok(json!({"findings":findings,"imported_path":state.import_path()}).to_string())
}

fn scan_agent_configs(root: &Path, max_chars: usize) -> Vec<InheritFinding> {
    let candidates = [
        ("codex-agents", "AGENTS.md"),
        ("codex-local", ".codex/AGENTS.md"),
        ("claude", "CLAUDE.md"),
        ("claude-local", ".claude/CLAUDE.md"),
        ("cursor-rules", ".cursor/rules"),
        ("windsurf-rules", ".windsurf/rules"),
        ("gemini", ".gemini"),
        ("cline", ".cline"),
        ("copilot", ".github/copilot-instructions.md"),
        ("vscode", ".vscode"),
    ];
    let mut out = Vec::new();
    for (surface, rel) in candidates {
        let path = root.join(rel);
        if path.is_file() {
            if let Ok(meta) = fs::metadata(&path) {
                out.push(InheritFinding {
                    surface: surface.into(),
                    path: path.clone(),
                    bytes: meta.len(),
                    excerpt: fs::read_to_string(&path)
                        .map(|s| truncate_chars(&s, max_chars))
                        .unwrap_or_default(),
                });
            }
        } else if path.is_dir() {
            for file in collect_text_files(&path, 30) {
                if let Ok(meta) = fs::metadata(&file) {
                    out.push(InheritFinding {
                        surface: surface.into(),
                        path: file.clone(),
                        bytes: meta.len(),
                        excerpt: fs::read_to_string(&file)
                            .map(|s| truncate_chars(&s, max_chars))
                            .unwrap_or_default(),
                    });
                }
            }
        }
    }
    out
}

async fn magic_benchmark(state: &MagicState) -> Result<String, ToolError> {
    let bench_dir = state.root.join("benchmark-smoke");
    let _ = fs::remove_dir_all(&bench_dir);
    fs::create_dir_all(&bench_dir).map_err(io_err("create benchmark dir"))?;
    let hash_file = bench_dir.join("hash.rs");
    fs::write(&hash_file, "fn alpha() {\n    println!(\"a\");\n}\n")
        .map_err(io_err("write hash fixture"))?;
    let hash = sha256_hex(&fs::read_to_string(&hash_file).map_err(io_err("read hash fixture"))?);
    let hash_result = hash_edit(json!({"path":hash_file,"expected_hash":&hash[..12],"old_string":"println!(\"a\");","new_string":"println!(\"b\");"})).await?;
    let conflict_file = bench_dir.join("conflict.txt");
    fs::write(
        &conflict_file,
        "one\n<<<<<<< ours\nleft\n=======\nright\n>>>>>>> theirs\ntwo\n",
    )
    .map_err(io_err("write conflict fixture"))?;
    let conflict_result = resolve_conflict(json!({"action":"list","path":conflict_file})).await?;
    let lsp_result =
        lsp_inspect(json!({"action":"symbols","path":bench_dir.join("hash.rs")})).await?;
    let guard_result =
        evaluate_stream_rules("I will implement this later", &default_stream_rules());
    let minimized = minimize_output(
        "cargo",
        "warning: unused\nerror[E0425]: missing\nlast\n",
        10,
        2_000,
    );
    Ok(json!({
        "version":MAGIC_VERSION,
        "hash_edit":serde_json::from_str::<Value>(&hash_result).unwrap_or_else(|_| json!(hash_result)),
        "conflicts":serde_json::from_str::<Value>(&conflict_result).unwrap_or_else(|_| json!(conflict_result)),
        "lsp":serde_json::from_str::<Value>(&lsp_result).unwrap_or_else(|_| json!(lsp_result)),
        "stream_rule_guard":guard_result,
        "minimize_output":minimized,
        "all_smokes_completed":true
    }).to_string())
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidParams(format!("Missing '{key}' parameter")))
}

fn bool_param(params: &Value, key: &str, default: bool) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn normalize_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn byte_range_for_lines(content: &str, start_line: usize, end_line: usize) -> (usize, usize) {
    let mut start = 0;
    let mut end = content.len();
    let mut line_no = 1;
    for (idx, ch) in content.char_indices() {
        if line_no == start_line {
            start = idx;
            break;
        }
        if ch == '\n' {
            line_no += 1;
        }
    }
    line_no = 1;
    for (idx, ch) in content.char_indices() {
        if line_no > end_line {
            end = idx;
            break;
        }
        if ch == '\n' {
            line_no += 1;
        }
    }
    (start, end)
}

fn paginate_lines(content: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return content.into();
    }
    content
        .lines()
        .skip(offset.unwrap_or(1).max(1) - 1)
        .take(limit.unwrap_or(usize::MAX).max(1))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.into();
    }
    let mut out = input.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
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

fn clean_join(base: &Path, rel: &str) -> PathBuf {
    clean_path(base.join(rel))
}

fn io_err(context: &'static str) -> impl FnOnce(std::io::Error) -> ToolError {
    move |e| ToolError::ExecutionFailed(format!("{context}: {e}"))
}

fn to_tool_err<E: std::fmt::Display>(e: E) -> ToolError {
    ToolError::ExecutionFailed(e.to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn collect_matching_files(root: &Path, needle: &str, out: &mut Vec<PathBuf>, max: usize) {
    if out.len() >= max || !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= max {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_matching_files(&path, needle, out, max);
        } else if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.contains(needle))
        {
            out.push(path);
        }
    }
}

fn collect_text_files(root: &Path, max: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= max {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if !should_skip_dir(&path) {
                    stack.push(path);
                }
            } else if ft.is_file() && is_text_path(&path) {
                out.push(path);
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    out.sort();
    out
}

fn collect_source_files(root: &Path, max: usize) -> Vec<PathBuf> {
    collect_text_files(root, max)
        .into_iter()
        .filter(|p| language_for_path(p) != "text")
        .collect()
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | ".next"
                    | "dist"
                    | "build"
                    | "__pycache__"
            )
        })
}

fn is_text_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "rs" | "py"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "go"
                    | "java"
                    | "kt"
                    | "swift"
                    | "md"
                    | "txt"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "json"
                    | "sh"
            )
        })
}

fn language_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        _ => "text",
    }
}

fn looks_like_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn split_once_or_all(input: &str, sep: char) -> (&str, &str) {
    input.split_once(sep).unwrap_or((input, ""))
}

fn safe_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn shell_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_edit_rejects_stale_hash_and_applies_prefix_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.rs");
        fs::write(&file, "fn main() {\n    println!(\"a\");\n}\n").unwrap();
        let stale = hash_edit(
            json!({"path":file,"expected_hash":"deadbeef","old_string":"a","new_string":"b"}),
        )
        .await
        .unwrap();
        assert!(stale.contains("stale_hash"));
        let current = sha256_hex(&fs::read_to_string(&file).unwrap());
        let applied = hash_edit(json!({"path":file,"expected_hash":&current[..10],"old_string":"println!(\"a\");","new_string":"println!(\"b\");"})).await.unwrap();
        assert!(applied.contains("\"applied\":true"));
        assert!(fs::read_to_string(&file)
            .unwrap()
            .contains("println!(\"b\");"));
    }

    #[tokio::test]
    async fn resource_reader_reads_file_and_searches_path() {
        let tmp = tempfile::tempdir().unwrap();
        let state = MagicState::new(tmp.path().join("state"));
        let file = tmp.path().join("note.txt");
        fs::write(&file, "alpha\nbeta\nalpha again\n").unwrap();
        let read = read_resource(
            json!({"uri":format!("file://{}", file.display()),"offset":2,"limit":1}),
            &state,
        )
        .await
        .unwrap();
        assert_eq!(read, "beta");
        let search = search_resource(json!({"uri":file,"pattern":"alpha"}), &state)
            .await
            .unwrap();
        assert!(search.contains("alpha again"));
    }

    #[tokio::test]
    async fn conflict_list_and_resolve_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("conflict.txt");
        fs::write(
            &file,
            "a\n<<<<<<< ours\nleft\n=======\nright\n>>>>>>> theirs\nz\n",
        )
        .unwrap();
        let listed = resolve_conflict(json!({"action":"list","path":file}))
            .await
            .unwrap();
        assert!(listed.contains("left"));
        let resolved = resolve_conflict(
            json!({"action":"resolve","path":tmp.path(),"index":0,"choice":"ours"}),
        )
        .await
        .unwrap();
        assert!(resolved.contains("\"resolved\":true"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "a\nleft\nz\n");
    }

    #[tokio::test]
    async fn lsp_symbols_references_and_rename_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("lib.rs");
        fs::write(
            &file,
            "pub struct Thing;\nfn use_it() { let _x = Thing; }\n",
        )
        .unwrap();
        let symbols = lsp_inspect(json!({"action":"symbols","path":file}))
            .await
            .unwrap();
        assert!(symbols.contains("Thing"));
        let refs = lsp_inspect(json!({"action":"references","path":tmp.path(),"symbol":"Thing"}))
            .await
            .unwrap();
        assert!(refs.contains("use_it"));
        let preview = lsp_inspect(json!({"action":"rename_preview","path":tmp.path(),"symbol":"Thing","replacement":"BetterThing"})).await.unwrap();
        assert!(preview.contains("BetterThing"));
        assert!(fs::read_to_string(&file).unwrap().contains("Thing"));
    }

    #[tokio::test]
    async fn transactions_accept_content_replace() {
        let tmp = tempfile::tempdir().unwrap();
        let state = MagicState::new(tmp.path().join("state"));
        let file = tmp.path().join("a.txt");
        fs::write(&file, "hello old\n").unwrap();
        let created = transaction_preview(json!({"action":"create","kind":"content_replace","payload":{"path":file,"old_string":"old","new_string":"new"}}), &state).await.unwrap();
        let id = serde_json::from_str::<Value>(&created).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        transaction_preview(json!({"action":"accept","id":id}), &state)
            .await
            .unwrap();
        assert!(fs::read_to_string(&file).unwrap().contains("hello new"));
    }

    #[tokio::test]
    async fn stream_rules_and_advisor_flag_unverified_claims() {
        let tmp = tempfile::tempdir().unwrap();
        let state = MagicState::new(tmp.path().join("state"));
        let eval = stream_rule_guard(
            json!({"action":"evaluate","text":"I will implement this later"}),
            &state,
        )
        .await
        .unwrap();
        assert!(eval.contains("no-status-only-completion"));
        let advice = advisor_watch(
            json!({"transcript":"Tests passed.","objective":"ship","evidence":""}),
            &state,
        )
        .await
        .unwrap();
        assert!(advice.contains("verification_required"));
    }

    #[tokio::test]
    async fn subagent_workspace_artifacts_are_addressable() {
        let tmp = tempfile::tempdir().unwrap();
        let state = MagicState::new(tmp.path().join("state"));
        let created = subagent_workspace(
            json!({"action":"create","id":"worker/one","goal":"check docs"}),
            &state,
        )
        .await
        .unwrap();
        assert!(created.contains("worker-one"));
        subagent_workspace(
            json!({"action":"write","id":"worker/one","path":"result.md","content":"ok"}),
            &state,
        )
        .await
        .unwrap();
        assert_eq!(
            read_agent_resource("worker-one/result.md", &state).unwrap(),
            "ok"
        );
    }

    #[test]
    fn output_minimizer_keeps_errors_warnings_and_tail() {
        let summary = minimize_output(
            "cargo",
            "line1\nwarning: unused\nline2\nerror[E0425]: nope\nlast\n",
            2,
            2000,
        );
        assert_eq!(summary.original_lines, 5);
        assert!(summary.minimized.contains("warning: unused"));
        assert!(summary.minimized.contains("error[E0425]"));
        assert!(summary.tail.contains(&"last".to_string()));
    }

    #[test]
    fn first_run_scan_finds_agent_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "rules").unwrap();
        let findings = scan_agent_configs(tmp.path(), 100);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].surface, "codex-agents");
    }

    #[tokio::test]
    async fn magic_benchmark_runs_smoke_suite() {
        let tmp = tempfile::tempdir().unwrap();
        let state = MagicState::new(tmp.path().join("state"));
        let result = magic_benchmark(&state).await.unwrap();
        assert!(result.contains("all_smokes_completed"));
        assert!(result.contains("hash_edit"));
    }
}
