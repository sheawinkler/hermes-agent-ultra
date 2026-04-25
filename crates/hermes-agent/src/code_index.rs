//! Workspace code indexing and repo-map rendering for always-on context.
//!
//! This module keeps a lightweight in-memory index of source files and
//! top-level symbols across common languages (Rust, Python, JS/TS, Go).
//! It is intentionally deterministic and bounded for prompt safety.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;

const DEFAULT_MAX_INDEXED_FILES: usize = 4_000;
const DEFAULT_MAX_FILE_BYTES: usize = 512 * 1024;
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 20;
const DEFAULT_REPO_MAP_MAX_FILES: usize = 32;
const DEFAULT_REPO_MAP_MAX_SYMBOLS: usize = 160;

/// A single indexed symbol occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub line: usize,
}

/// Reference match result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceHit {
    pub path: PathBuf,
    pub line: usize,
    pub snippet: String,
}

#[derive(Debug, Clone)]
struct IndexedFile {
    language: String,
    symbols: Vec<SymbolInfo>,
    lines: Vec<String>,
}

/// Code index configuration.
#[derive(Debug, Clone)]
pub struct CodeIndexConfig {
    pub enabled: bool,
    pub max_indexed_files: usize,
    pub max_file_bytes: usize,
    pub refresh_interval: Duration,
    pub repo_map_max_files: usize,
    pub repo_map_max_symbols: usize,
}

impl Default for CodeIndexConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_indexed_files: DEFAULT_MAX_INDEXED_FILES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            refresh_interval: Duration::from_secs(DEFAULT_REFRESH_INTERVAL_SECS),
            repo_map_max_files: DEFAULT_REPO_MAP_MAX_FILES,
            repo_map_max_symbols: DEFAULT_REPO_MAP_MAX_SYMBOLS,
        }
    }
}

impl CodeIndexConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(raw) = std::env::var("HERMES_CODE_INDEX_ENABLED") {
            let off = matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            );
            cfg.enabled = !off;
        }
        if let Some(v) = env_usize("HERMES_CODE_INDEX_MAX_FILES") {
            cfg.max_indexed_files = v.max(1);
        }
        if let Some(v) = env_usize("HERMES_CODE_INDEX_MAX_FILE_BYTES") {
            cfg.max_file_bytes = v.max(4 * 1024);
        }
        if let Some(v) = env_u64("HERMES_CODE_INDEX_REFRESH_SECS") {
            cfg.refresh_interval = Duration::from_secs(v.max(1));
        }
        if let Some(v) = env_usize("HERMES_REPO_MAP_MAX_FILES") {
            cfg.repo_map_max_files = v.max(1);
        }
        if let Some(v) = env_usize("HERMES_REPO_MAP_MAX_SYMBOLS") {
            cfg.repo_map_max_symbols = v.max(8);
        }
        cfg
    }
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// Summary stats from a refresh pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub files_scanned: usize,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
}

/// In-memory workspace index.
#[derive(Debug, Clone)]
pub struct CodeIndex {
    workspace_root: PathBuf,
    config: CodeIndexConfig,
    files: HashMap<PathBuf, IndexedFile>,
    last_refresh: Option<Instant>,
}

impl CodeIndex {
    pub fn new(workspace_root: impl Into<PathBuf>, config: CodeIndexConfig) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            config,
            files: HashMap::new(),
            last_refresh: None,
        }
    }

    pub fn default_for_workspace(workspace_root: impl Into<PathBuf>) -> Self {
        Self::new(workspace_root, CodeIndexConfig::from_env())
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn ensure_fresh(&mut self) -> IndexStats {
        if !self.config.enabled {
            return IndexStats::default();
        }
        let needs_refresh = self
            .last_refresh
            .map(|last| last.elapsed() >= self.config.refresh_interval)
            .unwrap_or(true);
        if needs_refresh {
            return self.refresh_full();
        }
        IndexStats::default()
    }

    pub fn refresh_full(&mut self) -> IndexStats {
        if !self.config.enabled {
            return IndexStats::default();
        }
        let mut stats = IndexStats::default();
        let candidates = collect_source_files(&self.workspace_root, self.config.max_indexed_files);
        stats.files_scanned = candidates.len();

        let mut next_files = HashMap::new();
        for path in candidates {
            let abs = if path.is_absolute() {
                path
            } else {
                self.workspace_root.join(path)
            };
            if let Some(indexed) = index_file(&abs, self.config.max_file_bytes) {
                stats.files_indexed += 1;
                stats.symbols_indexed += indexed.symbols.len();
                next_files.insert(abs, indexed);
            }
        }

        self.files = next_files;
        self.last_refresh = Some(Instant::now());
        stats
    }

    pub fn refresh_paths<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        if !self.config.enabled {
            return;
        }
        for path in paths {
            let abs = absolutize(&self.workspace_root, &path);
            if !is_indexable_source_path(&abs) {
                continue;
            }
            if let Some(indexed) = index_file(&abs, self.config.max_file_bytes) {
                self.files.insert(abs, indexed);
            } else {
                self.files.remove(&abs);
            }
        }
        self.last_refresh = Some(Instant::now());
    }

    pub fn list_file_symbols(&self, file_path: &Path, limit: usize) -> Vec<SymbolInfo> {
        let abs = absolutize(&self.workspace_root, file_path);
        self.files
            .get(&abs)
            .map(|f| f.symbols.iter().take(limit).cloned().collect())
            .unwrap_or_default()
    }

    pub fn find_references(&self, symbol: &str, limit: usize) -> Vec<ReferenceHit> {
        let sym = symbol.trim();
        if sym.is_empty() || !looks_like_identifier(sym) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let pat = format!(r"\b{}\b", regex::escape(sym));
        let Ok(re) = Regex::new(&pat) else {
            return Vec::new();
        };
        let mut paths: Vec<&PathBuf> = self.files.keys().collect();
        paths.sort();
        for path in paths {
            if let Some(file) = self.files.get(path) {
                for (idx, line) in file.lines.iter().enumerate() {
                    if re.is_match(line) {
                        out.push(ReferenceHit {
                            path: path.clone(),
                            line: idx + 1,
                            snippet: truncate(line.trim(), 180),
                        });
                        if out.len() >= limit {
                            return out;
                        }
                    }
                }
            }
        }
        out
    }

    pub fn render_repo_map(
        &mut self,
        max_files: Option<usize>,
        max_symbols: Option<usize>,
    ) -> String {
        if !self.config.enabled {
            return String::new();
        }
        let _ = self.ensure_fresh();

        let file_limit = max_files.unwrap_or(self.config.repo_map_max_files).max(1);
        let symbol_limit = max_symbols
            .unwrap_or(self.config.repo_map_max_symbols)
            .max(8);
        let mut symbol_budget_left = symbol_limit;
        let mut rows = BTreeMap::<String, Vec<SymbolInfo>>::new();

        let mut paths: Vec<PathBuf> = self.files.keys().cloned().collect();
        paths.sort();
        for abs in paths.into_iter().take(file_limit) {
            let rel = abs
                .strip_prefix(&self.workspace_root)
                .unwrap_or(&abs)
                .to_string_lossy()
                .to_string();
            let Some(file) = self.files.get(&abs) else {
                continue;
            };
            if symbol_budget_left == 0 {
                break;
            }
            let take = file.symbols.len().min(symbol_budget_left).min(14);
            symbol_budget_left = symbol_budget_left.saturating_sub(take);
            let symbols = file.symbols.iter().take(take).cloned().collect::<Vec<_>>();
            rows.insert(format!("{rel} ({})", file.language), symbols);
        }

        if rows.is_empty() {
            return String::new();
        }

        let mut out = String::from("## Repository Map (always-on code index)\n");
        for (file, symbols) in rows {
            out.push_str(&format!("- {file}\n"));
            if symbols.is_empty() {
                continue;
            }
            for sym in symbols {
                out.push_str(&format!(
                    "  - {} {} (line {})\n",
                    symbol_emoji(&sym.kind),
                    sym.name,
                    sym.line
                ));
            }
        }
        truncate(&out, 7_500)
    }
}

fn symbol_emoji(kind: &str) -> &'static str {
    match kind {
        "function" => "fn",
        "class" => "class",
        "struct" => "struct",
        "enum" => "enum",
        "trait" => "trait",
        "interface" => "iface",
        "method" => "method",
        "const" => "const",
        "type" => "type",
        _ => "sym",
    }
}

fn collect_source_files(root: &Path, max_files: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if should_skip_dir(&path) {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() && is_indexable_source_path(&path) {
                out.push(path);
                if out.len() >= max_files {
                    return out;
                }
            }
        }
    }
    out
}

fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | ".next"
            | ".cache"
            | "dist"
            | "build"
            | "__pycache__"
    )
}

fn is_indexable_source_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "go" | "java" | "kt" | "swift"
    )
}

fn language_for_ext(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        _ => "text",
    }
}

fn index_file(path: &Path, max_file_bytes: usize) -> Option<IndexedFile> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() as usize > max_file_bytes {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    let language = language_for_ext(path).to_string();
    let symbols = extract_symbols(path, &content);
    let lines = content.lines().map(|l| l.to_string()).collect::<Vec<_>>();
    Some(IndexedFile {
        language,
        symbols,
        lines,
    })
}

fn extract_symbols(path: &Path, content: &str) -> Vec<SymbolInfo> {
    let lang = language_for_ext(path);
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        match lang {
            "rust" => extract_rust_symbols(line, line_no, &mut out),
            "python" => extract_python_symbols(line, line_no, &mut out),
            "javascript" | "typescript" => extract_js_ts_symbols(line, line_no, &mut out),
            "go" => extract_go_symbols(line, line_no, &mut out),
            _ => {}
        }
    }
    out
}

fn extract_rust_symbols(line: &str, line_no: usize, out: &mut Vec<SymbolInfo>) {
    lazy_static::lazy_static! {
        static ref FN_RE: Regex = Regex::new(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        static ref STRUCT_RE: Regex = Regex::new(r"^\s*(?:pub\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        static ref ENUM_RE: Regex = Regex::new(r"^\s*(?:pub\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        static ref TRAIT_RE: Regex = Regex::new(r"^\s*(?:pub\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        static ref CONST_RE: Regex = Regex::new(r"^\s*(?:pub\s+)?const\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    }
    capture_symbol(line, line_no, "function", &FN_RE, out);
    capture_symbol(line, line_no, "struct", &STRUCT_RE, out);
    capture_symbol(line, line_no, "enum", &ENUM_RE, out);
    capture_symbol(line, line_no, "trait", &TRAIT_RE, out);
    capture_symbol(line, line_no, "const", &CONST_RE, out);
}

fn extract_python_symbols(line: &str, line_no: usize, out: &mut Vec<SymbolInfo>) {
    lazy_static::lazy_static! {
        static ref DEF_RE: Regex = Regex::new(r"^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
        static ref CLASS_RE: Regex = Regex::new(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    }
    capture_symbol(line, line_no, "function", &DEF_RE, out);
    capture_symbol(line, line_no, "class", &CLASS_RE, out);
}

fn extract_js_ts_symbols(line: &str, line_no: usize, out: &mut Vec<SymbolInfo>) {
    lazy_static::lazy_static! {
        static ref FN_RE: Regex = Regex::new(r"^\s*(?:export\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
        static ref CLASS_RE: Regex = Regex::new(r"^\s*(?:export\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        static ref CONST_RE: Regex = Regex::new(r"^\s*(?:export\s+)?const\s+([A-Za-z_][A-Za-z0-9_]*)\s*=").unwrap();
        static ref INTERFACE_RE: Regex = Regex::new(r"^\s*(?:export\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        static ref TYPE_RE: Regex = Regex::new(r"^\s*(?:export\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)\s*=").unwrap();
    }
    capture_symbol(line, line_no, "function", &FN_RE, out);
    capture_symbol(line, line_no, "class", &CLASS_RE, out);
    capture_symbol(line, line_no, "const", &CONST_RE, out);
    capture_symbol(line, line_no, "interface", &INTERFACE_RE, out);
    capture_symbol(line, line_no, "type", &TYPE_RE, out);
}

fn extract_go_symbols(line: &str, line_no: usize, out: &mut Vec<SymbolInfo>) {
    lazy_static::lazy_static! {
        static ref FUNC_RE: Regex = Regex::new(r"^\s*func\s+(?:\([^)]+\)\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
        static ref TYPE_RE: Regex = Regex::new(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)\s+").unwrap();
        static ref CONST_RE: Regex = Regex::new(r"^\s*const\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    }
    capture_symbol(line, line_no, "function", &FUNC_RE, out);
    capture_symbol(line, line_no, "type", &TYPE_RE, out);
    capture_symbol(line, line_no, "const", &CONST_RE, out);
}

fn capture_symbol(line: &str, line_no: usize, kind: &str, re: &Regex, out: &mut Vec<SymbolInfo>) {
    if let Some(caps) = re.captures(line) {
        if let Some(name) = caps.get(1).map(|m| m.as_str()) {
            out.push(SymbolInfo {
                name: name.to_string(),
                kind: kind.to_string(),
                line: line_no,
            });
        }
    }
}

fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out = input.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
}

fn looks_like_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn absolutize(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_symbol_extracts_top_level_items() {
        let src = r#"
pub struct AgentLoop {}
pub enum Mode { A, B }
pub trait Runner {}
pub async fn run_once() {}
const VERSION: &str = "1";
"#;
        let symbols = extract_symbols(Path::new("main.rs"), src);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AgentLoop"));
        assert!(names.contains(&"Mode"));
        assert!(names.contains(&"Runner"));
        assert!(names.contains(&"run_once"));
        assert!(names.contains(&"VERSION"));
    }

    #[test]
    fn python_and_typescript_symbol_extract() {
        let py = "class Foo:\n    pass\n\ndef bar(x):\n    return x\n";
        let ts = "export interface Item {}\nexport function run() {}\nexport const X = 1\n";
        let py_syms = extract_symbols(Path::new("a.py"), py);
        let ts_syms = extract_symbols(Path::new("a.ts"), ts);
        assert!(py_syms.iter().any(|s| s.name == "Foo"));
        assert!(py_syms.iter().any(|s| s.name == "bar"));
        assert!(ts_syms.iter().any(|s| s.name == "Item"));
        assert!(ts_syms.iter().any(|s| s.name == "run"));
        assert!(ts_syms.iter().any(|s| s.name == "X"));
    }

    #[test]
    fn reference_scan_matches_word_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.rs");
        fs::write(&p, "fn run() {}\nlet runner = run;\n").unwrap();
        let mut idx = CodeIndex::new(tmp.path(), CodeIndexConfig::default());
        idx.refresh_full();
        let refs = idx.find_references("run", 10);
        assert!(refs.iter().any(|r| r.line == 1));
        assert!(refs.iter().any(|r| r.line == 2));
    }
}
