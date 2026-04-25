//! LSP-style context injection hooks.
//!
//! This module augments the conversation with diagnostics + symbol/reference
//! hints after file tool calls. It is designed to be lightweight and
//! deterministic, while still giving the model immediate code-intel feedback.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use hermes_core::{ToolCall, ToolResult};

use crate::code_index::{CodeIndex, SymbolInfo};

const DEFAULT_MAX_CHARS: usize = 2_800;
const DEFAULT_MAX_REF_HITS: usize = 12;
const DEFAULT_MAX_SYMBOLS_PER_FILE: usize = 10;

#[derive(Debug, Clone)]
pub struct LspContextConfig {
    pub enabled: bool,
    pub diagnostics_on_write: bool,
    pub hover_on_read: bool,
    pub references_on_rename: bool,
    pub max_chars: usize,
    pub max_reference_hits: usize,
    pub max_symbols_per_file: usize,
}

impl Default for LspContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            diagnostics_on_write: true,
            hover_on_read: true,
            references_on_rename: true,
            max_chars: DEFAULT_MAX_CHARS,
            max_reference_hits: DEFAULT_MAX_REF_HITS,
            max_symbols_per_file: DEFAULT_MAX_SYMBOLS_PER_FILE,
        }
    }
}

impl LspContextConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        cfg.enabled = env_bool("HERMES_LSP_CONTEXT_ENABLED", true);
        cfg.diagnostics_on_write = env_bool("HERMES_LSP_DIAGNOSTICS_ON_WRITE", true);
        cfg.hover_on_read = env_bool("HERMES_LSP_HOVER_ON_READ", true);
        cfg.references_on_rename = env_bool("HERMES_LSP_REFERENCES_ON_RENAME", true);
        cfg.max_chars = env_usize("HERMES_LSP_CONTEXT_MAX_CHARS", DEFAULT_MAX_CHARS).max(400);
        cfg.max_reference_hits =
            env_usize("HERMES_LSP_REFERENCE_HITS", DEFAULT_MAX_REF_HITS).clamp(1, 100);
        cfg.max_symbols_per_file =
            env_usize("HERMES_LSP_SYMBOLS_PER_FILE", DEFAULT_MAX_SYMBOLS_PER_FILE).clamp(1, 40);
        cfg
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| {
            !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

/// Build a compact LSP-style context block from successful file-tool calls.
pub fn build_lsp_context_note(
    tool_calls: &[ToolCall],
    results: &[ToolResult],
    code_index: &mut CodeIndex,
    cfg: &LspContextConfig,
) -> Option<String> {
    if !cfg.enabled {
        return None;
    }

    let mut touched_paths = BTreeSet::<PathBuf>::new();
    let mut sections = Vec::<String>::new();

    for result in results {
        if result.is_error {
            continue;
        }
        let Some(call) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
            continue;
        };
        let Ok(args) = serde_json::from_str::<Value>(&call.function.arguments) else {
            continue;
        };

        match call.function.name.as_str() {
            "read_file" if cfg.hover_on_read => {
                if let Some(path) = parse_path_arg(&args) {
                    touched_paths.insert(path.clone());
                    if let Some(section) =
                        build_hover_section(code_index, &path, args.get("offset"), cfg)
                    {
                        sections.push(section);
                    }
                }
            }
            "write_file" | "patch_file" | "edit_file" => {
                if let Some(path) = parse_path_arg(&args) {
                    touched_paths.insert(path.clone());
                    if cfg.diagnostics_on_write {
                        if let Some(diag) = build_diagnostics_section(&path) {
                            sections.push(diag);
                        }
                    }
                    if cfg.references_on_rename && call.function.name == "patch_file" {
                        if let Some(refs) = build_reference_section(code_index, &args, cfg) {
                            sections.push(refs);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !touched_paths.is_empty() {
        code_index.refresh_paths(touched_paths.into_iter());
    }
    if sections.is_empty() {
        return None;
    }

    let mut out = String::from("## LSP Context Injection\n");
    for section in sections {
        out.push_str(&section);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Some(truncate(&out, cfg.max_chars))
}

fn parse_path_arg(args: &Value) -> Option<PathBuf> {
    args.get("path").and_then(|v| v.as_str()).map(PathBuf::from)
}

fn build_hover_section(
    code_index: &CodeIndex,
    path: &Path,
    read_offset: Option<&Value>,
    cfg: &LspContextConfig,
) -> Option<String> {
    let symbols = code_index.list_file_symbols(path, cfg.max_symbols_per_file);
    if symbols.is_empty() {
        return None;
    }
    let hint_line = read_offset
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .map(|v| v as usize);

    let focus = nearest_symbol(&symbols, hint_line);
    let mut block = format!("- Hover [{}]\n", path.display());
    if let Some(sym) = focus {
        block.push_str(&format!(
            "  - focus: {} {} (line {})\n",
            sym.kind, sym.name, sym.line
        ));
    }
    block.push_str("  - top symbols:");
    for sym in symbols.iter().take(5) {
        block.push_str(&format!(" {}:{}@{},", sym.kind, sym.name, sym.line));
    }
    if block.ends_with(',') {
        block.pop();
    }
    block.push('\n');
    Some(block)
}

fn nearest_symbol(symbols: &[SymbolInfo], line: Option<usize>) -> Option<&SymbolInfo> {
    let Some(target) = line else {
        return symbols.first();
    };
    symbols
        .iter()
        .filter(|s| s.line <= target)
        .max_by_key(|s| s.line)
        .or_else(|| symbols.first())
}

fn build_diagnostics_section(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some(diagnose_rust(path)),
        "py" => Some(diagnose_python(path)),
        _ => None,
    }
}

fn diagnose_rust(path: &Path) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return format!(
                "- Diagnostics [{}]\n  - rust: failed to read file: {}\n",
                path.display(),
                e
            );
        }
    };
    match syn::parse_file(&content) {
        Ok(_) => format!("- Diagnostics [{}]\n  - rust syntax: ok\n", path.display()),
        Err(e) => format!(
            "- Diagnostics [{}]\n  - rust syntax: error: {}\n",
            path.display(),
            e
        ),
    }
}

fn diagnose_python(path: &Path) -> String {
    let output = Command::new("python3")
        .args(["-m", "py_compile"])
        .arg(path)
        .output();
    match output {
        Ok(out) if out.status.success() => {
            format!(
                "- Diagnostics [{}]\n  - python syntax: ok\n",
                path.display()
            )
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            format!(
                "- Diagnostics [{}]\n  - python syntax: error: {}\n",
                path.display(),
                truncate(err.trim(), 240)
            )
        }
        Err(e) => format!(
            "- Diagnostics [{}]\n  - python syntax: check unavailable: {}\n",
            path.display(),
            e
        ),
    }
}

fn build_reference_section(
    code_index: &CodeIndex,
    args: &Value,
    cfg: &LspContextConfig,
) -> Option<String> {
    let old = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let new = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if old.is_empty() || new.is_empty() || old == new {
        return None;
    }
    if !looks_like_identifier(old) || !looks_like_identifier(new) {
        return None;
    }
    let refs = code_index.find_references(old, cfg.max_reference_hits);
    if refs.is_empty() {
        return None;
    }
    let mut block = format!("- References before rename `{old}` -> `{new}`\n");
    for hit in refs {
        block.push_str(&format!(
            "  - {}:{}: {}\n",
            hit.path.display(),
            hit.line,
            truncate(hit.snippet.trim(), 120)
        ));
    }
    Some(block)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_index::{CodeIndex, CodeIndexConfig};

    #[test]
    fn rename_reference_section_uses_identifier_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("m.rs");
        std::fs::write(&file, "fn old_name() {}\nlet x = old_name();\n").unwrap();
        let mut index = CodeIndex::new(tmp.path(), CodeIndexConfig::default());
        index.refresh_full();
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "old_string": "old_name",
            "new_string": "new_name"
        });
        let cfg = LspContextConfig::default();
        let section = build_reference_section(&index, &args, &cfg);
        assert!(section.unwrap_or_default().contains("old_name"));
    }
}
