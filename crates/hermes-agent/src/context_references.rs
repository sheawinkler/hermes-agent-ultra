//! `@` context reference preprocessing.
//!
//! Rust port of Python `agent/context_references.py` with the same security
//! model:
//! - `allowed_root` defaults to `cwd` (workspace confinement)
//! - sensitive home and Hermes credential paths are blocked
//! - attached context is bounded by soft/hard token budgets

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};

use hermes_intelligence::estimate_tokens_rough;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;

const TRAILING_PUNCTUATION: &str = ",.;!?";
const SENSITIVE_HOME_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube"];
const HIDDEN_SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "__pycache__"];
const SENSITIVE_HERMES_SUBDIR: &str = "skills/.hub";
const SENSITIVE_HOME_FILES: &[&str] = &[
    ".ssh/authorized_keys",
    ".ssh/id_rsa",
    ".ssh/id_ed25519",
    ".ssh/config",
    ".bashrc",
    ".zshrc",
    ".profile",
    ".bash_profile",
    ".zprofile",
    ".netrc",
    ".pgpass",
    ".npmrc",
    ".pypirc",
];
const MAX_FOLDER_LIST_ENTRIES: usize = 200;
const MAX_URL_CHARS: usize = 20_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextReference {
    pub raw: String,
    pub kind: String,
    pub target: String,
    pub start: usize,
    pub end: usize,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReferenceResult {
    pub message: String,
    pub original_message: String,
    pub references: Vec<ContextReference>,
    pub warnings: Vec<String>,
    pub injected_tokens: u64,
    pub expanded: bool,
    pub blocked: bool,
}

/// Parse context references from user message text.
pub fn parse_context_references(message: &str) -> Vec<ContextReference> {
    if message.is_empty() {
        return Vec::new();
    }

    let re =
        Regex::new(r"@(?:(?P<simple>diff|staged)\b|(?P<kind>file|folder|git|url):(?P<value>\S+))")
            .expect("valid context reference regex");
    let file_line_re =
        Regex::new(r"^(?P<path>.+?):(?P<start>\d+)(?:-(?P<end>\d+))?$").expect("valid line regex");

    let mut out = Vec::new();
    for caps in re.captures_iter(message) {
        let m = caps.get(0).expect("full match exists");
        if !is_valid_reference_start(message, m.start()) {
            continue;
        }
        if let Some(simple) = caps.name("simple").map(|v| v.as_str()) {
            out.push(ContextReference {
                raw: m.as_str().to_string(),
                kind: simple.to_string(),
                target: String::new(),
                start: m.start(),
                end: m.end(),
                line_start: None,
                line_end: None,
            });
            continue;
        }

        let kind = caps
            .name("kind")
            .map(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let value = strip_trailing_punctuation(
            caps.name("value")
                .map(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        );
        let mut target = value.clone();
        let mut line_start = None;
        let mut line_end = None;

        if kind == "file" {
            if let Some(file_caps) = file_line_re.captures(&value) {
                if let Some(path) = file_caps.name("path").map(|v| v.as_str()) {
                    target = path.to_string();
                }
                line_start = file_caps
                    .name("start")
                    .and_then(|v| v.as_str().parse::<usize>().ok());
                line_end = file_caps
                    .name("end")
                    .and_then(|v| v.as_str().parse::<usize>().ok())
                    .or(line_start);
            }
        }

        out.push(ContextReference {
            raw: m.as_str().to_string(),
            kind,
            target,
            start: m.start(),
            end: m.end(),
            line_start,
            line_end,
        });
    }

    out
}

/// Expand `@` references and inject attached context into the message.
pub async fn preprocess_context_references_async(
    message: &str,
    cwd: &Path,
    context_length: u64,
    allowed_root: Option<&Path>,
) -> ContextReferenceResult {
    let refs = parse_context_references(message);
    if refs.is_empty() {
        return ContextReferenceResult {
            message: message.to_string(),
            original_message: message.to_string(),
            references: refs,
            warnings: Vec::new(),
            injected_tokens: 0,
            expanded: false,
            blocked: false,
        };
    }

    let cwd_resolved = canonicalize_or_normalize(cwd);
    let allowed_root_path = allowed_root
        .map(canonicalize_or_normalize)
        .unwrap_or_else(|| cwd_resolved.clone());
    let mut warnings = Vec::new();
    let mut blocks = Vec::new();
    let mut injected_tokens = 0u64;

    for reference in &refs {
        let (warning, block) = expand_reference(reference, &cwd_resolved, &allowed_root_path).await;
        if let Some(w) = warning {
            warnings.push(w);
        }
        if let Some(b) = block {
            injected_tokens += estimate_tokens_rough(&b);
            blocks.push(b);
        }
    }

    let hard_limit = (context_length as f64 * 0.50) as u64;
    let soft_limit = (context_length as f64 * 0.25) as u64;

    if hard_limit > 0 && injected_tokens > hard_limit {
        warnings.push(format!(
            "@ context injection refused: {injected_tokens} tokens exceeds the 50% hard limit ({hard_limit})."
        ));
        return ContextReferenceResult {
            message: message.to_string(),
            original_message: message.to_string(),
            references: refs,
            warnings,
            injected_tokens,
            expanded: false,
            blocked: true,
        };
    }

    if soft_limit > 0 && injected_tokens > soft_limit {
        warnings.push(format!(
            "@ context injection warning: {injected_tokens} tokens exceeds the 25% soft limit ({soft_limit})."
        ));
    }

    let mut final_message = remove_reference_tokens(message, &refs);
    if !warnings.is_empty() {
        final_message.push_str("\n\n--- Context Warnings ---\n");
        for warning in &warnings {
            final_message.push_str("- ");
            final_message.push_str(warning);
            final_message.push('\n');
        }
        final_message = final_message.trim_end().to_string();
    }
    if !blocks.is_empty() {
        final_message.push_str("\n\n--- Attached Context ---\n\n");
        final_message.push_str(&blocks.join("\n\n"));
    }

    ContextReferenceResult {
        message: final_message.trim().to_string(),
        original_message: message.to_string(),
        references: refs,
        warnings,
        injected_tokens,
        expanded: true,
        blocked: false,
    }
}

async fn expand_reference(
    reference: &ContextReference,
    cwd: &Path,
    allowed_root: &Path,
) -> (Option<String>, Option<String>) {
    match reference.kind.as_str() {
        "file" => expand_file_reference(reference, cwd, allowed_root).await,
        "folder" => expand_folder_reference(reference, cwd, allowed_root).await,
        "diff" => expand_git_reference(reference, cwd, &["diff"], "git diff").await,
        "staged" => {
            expand_git_reference(reference, cwd, &["diff", "--staged"], "git diff --staged").await
        }
        "git" => {
            let count = reference
                .target
                .parse::<u32>()
                .ok()
                .map(|v| v.clamp(1, 10))
                .unwrap_or(1);
            let arg = format!("-{count}");
            expand_git_reference(
                reference,
                cwd,
                &["log", &arg, "-p"],
                &format!("git log {arg} -p"),
            )
            .await
        }
        "url" => expand_url_reference(reference).await,
        _ => (
            Some(format!(
                "{}: unsupported reference type '{}'",
                reference.raw, reference.kind
            )),
            None,
        ),
    }
}

async fn expand_file_reference(
    reference: &ContextReference,
    cwd: &Path,
    allowed_root: &Path,
) -> (Option<String>, Option<String>) {
    let resolved = match resolve_path(cwd, &reference.target, Some(allowed_root)) {
        Ok(p) => p,
        Err(e) => return (Some(format!("{}: {}", reference.raw, e)), None),
    };

    if let Err(e) = ensure_reference_path_allowed(&resolved) {
        return (Some(format!("{}: {}", reference.raw, e)), None);
    }
    if !resolved.exists() {
        return (Some(format!("{}: file not found", reference.raw)), None);
    }
    if !resolved.is_file() {
        return (Some(format!("{}: path is not a file", reference.raw)), None);
    }
    if is_binary_file(&resolved) {
        return (
            Some(format!("{}: binary files are not supported", reference.raw)),
            None,
        );
    }

    let text = match tokio::fs::read_to_string(&resolved).await {
        Ok(v) => v,
        Err(e) => {
            return (
                Some(format!("{}: failed to read file ({e})", reference.raw)),
                None,
            )
        }
    };
    let selected = if let Some(start) = reference.line_start {
        let end = reference.line_end.unwrap_or(start);
        select_line_range(&text, start, end)
    } else {
        text
    };

    let lang = code_fence_language(&resolved);
    let tokens = estimate_tokens_rough(&selected);
    let block = format!(
        "📄 {} ({tokens} tokens)\n```{lang}\n{}\n```",
        reference.raw, selected
    );
    (None, Some(block))
}

async fn expand_folder_reference(
    reference: &ContextReference,
    cwd: &Path,
    allowed_root: &Path,
) -> (Option<String>, Option<String>) {
    let resolved = match resolve_path(cwd, &reference.target, Some(allowed_root)) {
        Ok(p) => p,
        Err(e) => return (Some(format!("{}: {}", reference.raw, e)), None),
    };
    if let Err(e) = ensure_reference_path_allowed(&resolved) {
        return (Some(format!("{}: {}", reference.raw, e)), None);
    }
    if !resolved.exists() {
        return (Some(format!("{}: folder not found", reference.raw)), None);
    }
    if !resolved.is_dir() {
        return (
            Some(format!("{}: path is not a folder", reference.raw)),
            None,
        );
    }

    let listing = build_folder_listing(&resolved, cwd, MAX_FOLDER_LIST_ENTRIES);
    let tokens = estimate_tokens_rough(&listing);
    let block = format!("📁 {} ({tokens} tokens)\n{}", reference.raw, listing);
    (None, Some(block))
}

async fn expand_git_reference(
    reference: &ContextReference,
    cwd: &Path,
    args: &[&str],
    label: &str,
) -> (Option<String>, Option<String>) {
    let output = match TokioCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return (
                Some(format!("{}: git command failed ({e})", reference.raw)),
                None,
            )
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let msg = if stderr.is_empty() {
            "git command failed".to_string()
        } else {
            stderr
        };
        return (Some(format!("{}: {msg}", reference.raw)), None);
    }
    let content = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let final_content = if content.is_empty() {
        "(no output)".to_string()
    } else {
        content
    };
    let tokens = estimate_tokens_rough(&final_content);
    let block = format!("🧾 {label} ({tokens} tokens)\n```diff\n{final_content}\n```");
    (None, Some(block))
}

async fn expand_url_reference(reference: &ContextReference) -> (Option<String>, Option<String>) {
    let target = reference.target.trim();
    if target.is_empty() {
        return (Some(format!("{}: empty URL", reference.raw)), None);
    }

    let response = match reqwest::Client::new().get(target).send().await {
        Ok(v) => v,
        Err(e) => {
            return (
                Some(format!("{}: failed to fetch URL ({e})", reference.raw)),
                None,
            )
        }
    };
    let status = response.status();
    if !status.is_success() {
        return (
            Some(format!(
                "{}: failed to fetch URL (HTTP {})",
                reference.raw, status
            )),
            None,
        );
    }
    let body = match response.text().await {
        Ok(v) => v,
        Err(e) => {
            return (
                Some(format!("{}: failed to read URL body ({e})", reference.raw)),
                None,
            )
        }
    };

    let content = sanitize_html_to_text(&body);
    if content.is_empty() {
        return (
            Some(format!("{}: no content extracted", reference.raw)),
            None,
        );
    }
    let tokens = estimate_tokens_rough(&content);
    let block = format!("🌐 {} ({tokens} tokens)\n{}", reference.raw, content);
    (None, Some(block))
}

fn sanitize_html_to_text(body: &str) -> String {
    let no_script = Regex::new(r"(?is)<script[^>]*>.*?</script>")
        .expect("valid script regex")
        .replace_all(body, " ");
    let no_style = Regex::new(r"(?is)<style[^>]*>.*?</style>")
        .expect("valid style regex")
        .replace_all(&no_script, " ");
    let stripped = Regex::new(r"(?is)<[^>]+>")
        .expect("valid html tag regex")
        .replace_all(&no_style, " ");
    let normalized = Regex::new(r"\s+")
        .expect("valid whitespace regex")
        .replace_all(stripped.trim(), " ");
    normalized.chars().take(MAX_URL_CHARS).collect::<String>()
}

fn remove_reference_tokens(message: &str, refs: &[ContextReference]) -> String {
    if refs.is_empty() {
        return message.trim().to_string();
    }
    let mut parts = String::new();
    let mut cursor = 0usize;
    for reference in refs {
        if reference.start >= cursor {
            parts.push_str(&message[cursor..reference.start]);
            cursor = reference.end;
        }
    }
    if cursor <= message.len() {
        parts.push_str(&message[cursor..]);
    }
    let collapsed = Regex::new(r"\s{2,}")
        .expect("valid collapse regex")
        .replace_all(parts.trim(), " ");
    let fixed_punct = Regex::new(r"\s+([,.;:!?])")
        .expect("valid punctuation regex")
        .replace_all(&collapsed, "$1");
    fixed_punct.trim().to_string()
}

fn resolve_path(cwd: &Path, target: &str, allowed_root: Option<&Path>) -> Result<PathBuf, String> {
    let expanded = expand_user_path(target);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    let resolved = canonicalize_or_normalize(&joined);

    if let Some(root) = allowed_root {
        let root_norm = canonicalize_or_normalize(root);
        if !resolved.starts_with(&root_norm) {
            return Err("path is outside the allowed workspace".to_string());
        }
    }
    Ok(resolved)
}

fn expand_user_path(input: &str) -> PathBuf {
    if !input.starts_with('~') {
        return PathBuf::from(input);
    }
    let rest = &input[1..];
    let home = home_dir();

    if rest.is_empty() {
        return home.unwrap_or_else(|| PathBuf::from(input));
    }
    if rest.starts_with('/') {
        if let Some(home) = home {
            let suffix = rest.trim_start_matches('/');
            return if suffix.is_empty() {
                home
            } else {
                home.join(suffix)
            };
        }
        return PathBuf::from(input);
    }

    let (username, suffix) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };
    if !is_valid_username(username) {
        return PathBuf::from(input);
    }

    if let Some(home_for_user) = lookup_home_for_username(username) {
        return if suffix.is_empty() {
            home_for_user
        } else {
            home_for_user.join(suffix)
        };
    }
    PathBuf::from(input)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn current_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("LOGNAME")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("USERNAME")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
}

fn is_valid_username(username: &str) -> bool {
    !username.is_empty()
        && username
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[cfg(unix)]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        let user = parts.next()?;
        let _passwd = parts.next()?;
        let _uid = parts.next()?;
        let _gid = parts.next()?;
        let _gecos = parts.next()?;
        let home = parts.next()?;
        if user == username {
            return Some(PathBuf::from(home));
        }
    }
    None
}

#[cfg(not(unix))]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    None
}

fn ensure_reference_path_allowed(path: &Path) -> Result<(), String> {
    let home = canonicalize_or_normalize(
        &home_dir().ok_or_else(|| "home directory is unavailable".to_string())?,
    );
    let hermes_home = canonicalize_or_normalize(
        &std::env::var("HERMES_HOME")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".hermes")),
    );

    let mut blocked_exact: Vec<PathBuf> = SENSITIVE_HOME_FILES
        .iter()
        .map(|rel| home.join(rel))
        .collect();
    blocked_exact.push(hermes_home.join(".env"));
    if blocked_exact.iter().any(|blocked| path == blocked) {
        return Err("path is a sensitive credential file and cannot be attached".to_string());
    }

    let mut blocked_dirs: Vec<PathBuf> = SENSITIVE_HOME_DIRS
        .iter()
        .map(|rel| home.join(rel))
        .collect();
    blocked_dirs.push(hermes_home.join(SENSITIVE_HERMES_SUBDIR));
    if blocked_dirs.iter().any(|dir| path.starts_with(dir)) {
        return Err(
            "path is a sensitive credential or internal Hermes path and cannot be attached"
                .to_string(),
        );
    }
    Ok(())
}

fn canonicalize_or_normalize(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(v) => v,
        Err(_) => normalize_path_lexical(path),
    }
}

fn normalize_path_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn is_valid_reference_start(message: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    message[..start]
        .chars()
        .next_back()
        .map(|ch| !(ch.is_alphanumeric() || ch == '_' || ch == '/'))
        .unwrap_or(true)
}

fn strip_trailing_punctuation(mut value: String) -> String {
    while value
        .chars()
        .last()
        .is_some_and(|ch| TRAILING_PUNCTUATION.contains(ch))
    {
        value.pop();
    }
    loop {
        let Some(last) = value.chars().last() else {
            break;
        };
        let opener = match last {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        let closer_count = value.chars().filter(|c| *c == last).count();
        let opener_count = value.chars().filter(|c| *c == opener).count();
        if closer_count > opener_count {
            value.pop();
            continue;
        }
        break;
    }
    value
}

fn select_line_range(text: &str, start_line: usize, end_line: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = start_line
        .saturating_sub(1)
        .min(lines.len().saturating_sub(1));
    let end = end_line.max(start_line).min(lines.len());
    lines[start..end].join("\n")
}

fn is_binary_file(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    bytes.iter().take(4096).any(|b| *b == 0)
}

fn code_fence_language(path: &Path) -> &'static str {
    match path.extension().and_then(|v| v.to_str()).unwrap_or("") {
        "rs" => "rust",
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" => "markdown",
        "sh" => "bash",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        _ => "",
    }
}

fn build_folder_listing(path: &Path, cwd: &Path, limit: usize) -> String {
    let base = path
        .strip_prefix(cwd)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf());
    let base_display = if base.as_os_str().is_empty() {
        ".".to_string()
    } else {
        base.display().to_string()
    };

    let mut lines = vec![format!("{base_display}/")];
    let base_depth = base.components().count();
    let mut queue = VecDeque::new();
    queue.push_back(path.to_path_buf());
    let mut seen_count = 0usize;

    while let Some(dir) = queue.pop_front() {
        let mut entries = match std::fs::read_dir(&dir) {
            Ok(rd) => rd.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(_) => continue,
        };
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            if seen_count >= limit {
                lines.push("- ...".to_string());
                return lines.join("\n");
            }
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if HIDDEN_SKIP_DIRS.iter().any(|skip| *skip == name) {
                continue;
            }
            if name.starts_with('.') {
                continue;
            }

            let rel = p
                .strip_prefix(cwd)
                .map(|v| v.to_path_buf())
                .unwrap_or_else(|_| p.clone());
            let depth = rel.components().count().saturating_sub(base_depth + 1);
            let indent = "  ".repeat(depth);

            if p.is_dir() {
                lines.push(format!("{indent}- {name}/"));
                queue.push_back(p);
            } else {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                lines.push(format!("{indent}- {name} ({size} bytes)"));
            }
            seen_count += 1;
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use tempfile::tempdir;

    struct EnvGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[tokio::test]
    async fn defaults_allowed_root_to_cwd() {
        let td = tempdir().unwrap();
        let workspace = td.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let secret = td.path().join("secret.txt");
        std::fs::write(&secret, "outside\n").unwrap();

        let msg = format!("read @file:{}", secret.display());
        let result = preprocess_context_references_async(&msg, &workspace, 100_000, None).await;
        assert!(result.expanded);
        assert!(!result.message.contains("```outside"));
        assert!(!result.message.contains("\noutside\n```"));
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("outside the allowed workspace")));
    }

    #[tokio::test]
    async fn blocks_sensitive_home_and_hermes_paths() {
        let td = tempdir().unwrap();
        let _home_guard = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
        let _hermes_guard = EnvGuard::set(
            "HERMES_HOME",
            td.path().join(".hermes").to_string_lossy().as_ref(),
        );

        let hermes_env = td.path().join(".hermes/.env");
        std::fs::create_dir_all(hermes_env.parent().unwrap()).unwrap();
        std::fs::write(&hermes_env, "API_KEY=secret\n").unwrap();

        let ssh_key = td.path().join(".ssh/id_rsa");
        std::fs::create_dir_all(ssh_key.parent().unwrap()).unwrap();
        std::fs::write(&ssh_key, "PRIVATE-KEY\n").unwrap();

        let result = preprocess_context_references_async(
            "read @file:.hermes/.env and @file:.ssh/id_rsa",
            td.path(),
            100_000,
            Some(td.path()),
        )
        .await;

        assert!(result.expanded);
        assert!(!result.message.contains("API_KEY=secret"));
        assert!(!result.message.contains("PRIVATE-KEY"));
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("sensitive credential")));
    }

    #[tokio::test]
    async fn expands_valid_workspace_file_reference() {
        let td = tempdir().unwrap();
        let source = td.path().join("src/lib.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "fn main() {}\nlet x = 1;\n").unwrap();

        let msg = format!("summarize @file:{}:1-1 please", source.display());
        let result = preprocess_context_references_async(&msg, td.path(), 100_000, None).await;
        assert!(result.message.contains("Attached Context"));
        assert!(result.message.contains("fn main() {}"));
        assert!(!result.message.contains("let x = 1;"));
    }

    #[test]
    fn parse_skips_embedded_reference_tokens() {
        let refs = parse_context_references("foo/bar@file:test.txt and @file:ok.txt");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "ok.txt");
    }
}
