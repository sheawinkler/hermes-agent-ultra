//! Real file tool backends: patch (fuzzy match) and search (regex/glob).

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::tools::file::{PatchBackend, SearchBackend};
use crate::tools::fuzzy_match::{format_no_match_hint, fuzzy_find_and_replace};
use hermes_core::ToolError;

// ---------------------------------------------------------------------------
// LocalPatchBackend
// ---------------------------------------------------------------------------

/// Real file patch backend using fuzzy string matching.
///
/// Implements an 8-strategy matching chain (ported from Python `fuzzy_match.py`):
/// 1. Exact match
/// 2. Line-trimmed (strip leading/trailing whitespace per line)
/// 3. Whitespace normalized (collapse multiple spaces/tabs)
/// 4. Indentation flexible (ignore indentation differences)
/// 5. Escape normalized (convert \\n literals to newlines)
/// 6. Trimmed boundary (trim first/last line only)
/// 7. Block anchor (match first+last lines, similarity for middle)
/// 8. Context-aware (50% line similarity threshold)
pub struct LocalPatchBackend;

impl LocalPatchBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalPatchBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PatchBackend for LocalPatchBackend {
    async fn patch_file(
        &self,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<String, ToolError> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read '{}': {}", path, e)))?;

        if old_string.is_empty() {
            // Empty old_string means create new file or append
            let new_content = if content.is_empty() {
                new_string.to_string()
            } else {
                format!("{}\n{}", content, new_string)
            };
            tokio::fs::write(path, &new_content).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
            })?;
            return Ok(json!({"status": "ok", "message": "Content appended"}).to_string());
        }

        let result = fuzzy_find_and_replace(&content, old_string, new_string, replace_all);
        if let Some(error) = result.error {
            let hint = format_no_match_hint(Some(&error), result.match_count, old_string, &content);
            return Err(ToolError::ExecutionFailed(format!("{error}{hint}")));
        }

        tokio::fs::write(path, &result.content).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
        })?;

        Ok(json!({
            "status": "ok",
            "replacements": result.match_count,
            "strategy": result.strategy,
        })
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// LocalSearchBackend
// ---------------------------------------------------------------------------

/// Real file search backend using regex for content and glob for filenames.
pub struct LocalSearchBackend;

impl LocalSearchBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalSearchBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalSearchBackend {
    fn has_rg_command() -> bool {
        Command::new("rg")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn rg_file_glob(pattern: &str) -> String {
        if !pattern.contains('/') && !pattern.starts_with('*') {
            format!("*{pattern}")
        } else {
            pattern.to_string()
        }
    }

    fn search_files_rg_sync(
        pattern: &str,
        path: &str,
        max: usize,
        offset: usize,
    ) -> Result<String, ToolError> {
        let glob_pattern = Self::rg_file_glob(pattern);
        let run = |sorted: bool| -> Result<Vec<String>, ToolError> {
            let mut cmd = Command::new("rg");
            cmd.arg("--files");
            if sorted {
                cmd.arg("--sortr=modified");
            }
            cmd.arg("-g").arg(&glob_pattern);
            cmd.arg(path);
            let output = cmd.output().map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "File search requires 'rg' (ripgrep). Install from https://github.com/BurntSushi/ripgrep#installation ({e})"
                ))
            })?;
            if output.status.code() == Some(2) {
                let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(ToolError::ExecutionFailed(format!("Search failed: {err}")));
            }
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect())
        };
        let all_files = run(true).or_else(|_| run(false))?;
        let total = all_files.len();
        let page: Vec<Value> = all_files
            .into_iter()
            .skip(offset)
            .take(max)
            .map(|path_str| {
                let p = PathBuf::from(&path_str);
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let is_dir = p.is_dir();
                json!({
                    "path": path_str,
                    "name": name,
                    "is_dir": is_dir,
                })
            })
            .collect();
        Ok(json!({
            "files": page,
            "total": total,
            "pattern": pattern,
            "truncated": total > offset.saturating_add(max),
        })
        .to_string())
    }

    fn search_content_rg_sync(
        pattern: &str,
        path: &str,
        file_glob: Option<&str>,
        max: usize,
        offset: usize,
        output_mode: &str,
        context: usize,
    ) -> Result<String, ToolError> {
        let mut cmd = Command::new("rg");
        cmd.arg("--line-number")
            .arg("--no-heading")
            .arg("--with-filename");
        if context > 0 {
            cmd.arg("-C").arg(context.to_string());
        }
        if let Some(glob) = file_glob {
            cmd.arg("--glob").arg(glob);
        }
        match output_mode {
            "files_only" => {
                cmd.arg("-l");
            }
            "count" => {
                cmd.arg("-c");
            }
            _ => {}
        }
        cmd.arg(pattern).arg(path);
        let output = cmd.output().map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Content search requires ripgrep (rg) or grep. Install ripgrep: https://github.com/BurntSushi/ripgrep#installation ({e})"
            ))
        })?;
        if output.status.code() == Some(2) && output.stdout.is_empty() {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ToolError::ExecutionFailed(format!("Search failed: {err}")));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let fetch_limit = if context > 0 {
            max.saturating_add(offset).saturating_add(200)
        } else {
            max.saturating_add(offset)
        };

        match output_mode {
            "files_only" => {
                let all_files: Vec<String> = stdout
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                let total = all_files.len();
                let page: Vec<String> = all_files.into_iter().skip(offset).take(max).collect();
                Ok(json!({
                    "files": page,
                    "total": total,
                    "pattern": pattern,
                })
                .to_string())
            }
            "count" => {
                let mut counts = BTreeMap::new();
                for line in stdout.lines().filter(|l| !l.is_empty()) {
                    if let Some((file, count)) = line.rsplit_once(':') {
                        if let Ok(n) = count.parse::<usize>() {
                            counts.insert(file.to_string(), n);
                        }
                    }
                }
                let total_count: usize = counts.values().sum();
                Ok(json!({
                    "counts": counts,
                    "total": total_count,
                    "pattern": pattern,
                })
                .to_string())
            }
            _ => {
                let match_re =
                    Regex::new(r"^([A-Za-z]:)?(.*?):(\d+):(.*)$").expect("rg match line regex");
                let context_re =
                    Regex::new(r"^([A-Za-z]:)?(.*?)-(\d+)-(.*)$").expect("rg context line regex");
                let mut matches = Vec::new();
                for line in stdout.lines() {
                    if line.is_empty() || line == "--" {
                        continue;
                    }
                    if let Some(caps) = match_re.captures(line) {
                        let path_part = format!(
                            "{}{}",
                            caps.get(1).map(|m| m.as_str()).unwrap_or(""),
                            caps.get(2).map(|m| m.as_str()).unwrap_or("")
                        );
                        let line_no: usize = caps
                            .get(3)
                            .and_then(|m| m.as_str().parse().ok())
                            .unwrap_or(0);
                        let content = caps
                            .get(4)
                            .map(|m| m.as_str().chars().take(500).collect::<String>())
                            .unwrap_or_default();
                        matches.push(json!({
                            "file": path_part,
                            "line": line_no,
                            "content": content,
                        }));
                    } else if context > 0 {
                        if let Some(caps) = context_re.captures(line) {
                            let path_part = format!(
                                "{}{}",
                                caps.get(1).map(|m| m.as_str()).unwrap_or(""),
                                caps.get(2).map(|m| m.as_str()).unwrap_or("")
                            );
                            let line_no: usize = caps
                                .get(3)
                                .and_then(|m| m.as_str().parse().ok())
                                .unwrap_or(0);
                            let content = caps
                                .get(4)
                                .map(|m| m.as_str().chars().take(500).collect::<String>())
                                .unwrap_or_default();
                            matches.push(json!({
                                "file": path_part,
                                "line": line_no,
                                "content": content,
                            }));
                        }
                    }
                    if matches.len() >= fetch_limit {
                        break;
                    }
                }
                let total = matches.len();
                let page: Vec<Value> = matches.into_iter().skip(offset).take(max).collect();
                Ok(json!({
                    "matches": page,
                    "total": total,
                    "pattern": pattern,
                    "truncated": total > offset.saturating_add(max),
                })
                .to_string())
            }
        }
    }
}

#[async_trait]
impl SearchBackend for LocalSearchBackend {
    async fn search_content(
        &self,
        pattern: &str,
        path: &str,
        file_glob: Option<&str>,
        max_results: Option<usize>,
        offset: Option<usize>,
        output_mode: Option<&str>,
        context: Option<usize>,
    ) -> Result<String, ToolError> {
        let path_obj = Path::new(path);
        if !path_obj.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "Path '{}' does not exist",
                path_obj.display()
            )));
        }

        let max = max_results.unwrap_or(50);
        let offset = offset.unwrap_or(0);
        let output_mode = output_mode.unwrap_or("content");
        let context = context.unwrap_or(0);

        if Self::has_rg_command() {
            let pattern_owned = pattern.to_string();
            let path_owned = path.to_string();
            let file_glob_owned = file_glob.map(str::to_string);
            let output_mode_owned = output_mode.to_string();
            return tokio::task::spawn_blocking(move || {
                Self::search_content_rg_sync(
                    &pattern_owned,
                    &path_owned,
                    file_glob_owned.as_deref(),
                    max,
                    offset,
                    &output_mode_owned,
                    context,
                )
            })
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("rg search task failed: {e}")))?;
        }

        let re = Regex::new(pattern)
            .map_err(|e| ToolError::InvalidParams(format!("Invalid regex pattern: {}", e)))?;

        let fetch_limit = if context > 0 {
            max.saturating_add(offset).saturating_add(200)
        } else {
            max.saturating_add(offset)
        };

        let mut matches: Vec<Value> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();

        let file_glob_re = match file_glob {
            Some(glob) => Some(Self::compile_glob_regex(glob)?),
            None => None,
        };

        Self::search_dir_content(
            &re,
            path_obj,
            file_glob_re.as_ref(),
            context,
            fetch_limit,
            &mut matches,
            &mut files,
            &mut seen_files,
            &mut counts,
        )
        .await;

        match output_mode {
            "files_only" => {
                let total = files.len();
                let page: Vec<String> = files.into_iter().skip(offset).take(max).collect();
                Ok(json!({
                    "files": page,
                    "total": total,
                    "pattern": pattern,
                })
                .to_string())
            }
            "count" => {
                let total_count: usize = counts.values().sum();
                Ok(json!({
                    "counts": counts,
                    "total": total_count,
                    "pattern": pattern,
                })
                .to_string())
            }
            _ => {
                let total = matches.len();
                let page: Vec<Value> = matches.into_iter().skip(offset).take(max).collect();
                Ok(json!({
                    "matches": page,
                    "total": total,
                    "pattern": pattern,
                    "truncated": total > offset.saturating_add(max),
                })
                .to_string())
            }
        }
    }

    async fn search_files(
        &self,
        pattern: &str,
        path: &str,
        max_results: Option<usize>,
        offset: Option<usize>,
    ) -> Result<String, ToolError> {
        let base = std::path::Path::new(path);

        if !base.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "Path '{}' does not exist",
                base.display()
            )));
        }

        let max = max_results.unwrap_or(50);
        let offset = offset.unwrap_or(0);

        if Self::has_rg_command() {
            let pattern_owned = pattern.to_string();
            let path_owned = path.to_string();
            return tokio::task::spawn_blocking(move || {
                Self::search_files_rg_sync(&pattern_owned, &path_owned, max, offset)
            })
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("rg files task failed: {e}")))?;
        }

        let mut results: Vec<Value> = Vec::new();
        let glob_re = Self::compile_glob_regex(pattern)?;
        Self::search_dir_names(&glob_re, base, &mut results).await;

        let total = results.len();
        let page: Vec<Value> = results.into_iter().skip(offset).take(max).collect();

        Ok(json!({
            "files": page,
            "total": total,
            "pattern": pattern,
            "truncated": total > offset.saturating_add(max),
        })
        .to_string())
    }
}

const MAX_SEARCH_DEPTH: u32 = 12;

impl LocalSearchBackend {
    async fn search_dir_content(
        re: &Regex,
        dir: &std::path::Path,
        file_glob_re: Option<&Regex>,
        context: usize,
        fetch_limit: usize,
        matches: &mut Vec<Value>,
        files: &mut Vec<String>,
        seen_files: &mut HashSet<String>,
        counts: &mut BTreeMap<String, usize>,
    ) {
        Self::search_dir_content_depth(
            re,
            dir,
            file_glob_re,
            context,
            fetch_limit,
            matches,
            files,
            seen_files,
            counts,
            0,
        )
        .await;
    }

    async fn search_dir_content_depth(
        re: &Regex,
        dir: &std::path::Path,
        file_glob_re: Option<&Regex>,
        context: usize,
        fetch_limit: usize,
        matches: &mut Vec<Value>,
        files: &mut Vec<String>,
        seen_files: &mut HashSet<String>,
        counts: &mut BTreeMap<String, usize>,
        depth: u32,
    ) {
        if matches.len() >= fetch_limit || depth >= MAX_SEARCH_DEPTH {
            return;
        }

        let entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut entries = entries;
        while let Ok(Some(entry)) = entries.next_entry().await {
            if matches.len() >= fetch_limit {
                break;
            }

            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || name == "__pycache__"
            {
                continue;
            }

            if path.is_dir() {
                Box::pin(Self::search_dir_content_depth(
                    re,
                    &path,
                    file_glob_re,
                    context,
                    fetch_limit,
                    matches,
                    files,
                    seen_files,
                    counts,
                    depth + 1,
                ))
                .await;
            } else if path.is_file() {
                // Check glob filter
                if let Some(glob_re) = file_glob_re {
                    if !glob_re.is_match(name) {
                        continue;
                    }
                }

                // Try to read as text
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                    let lines: Vec<&str> = content.lines().collect();
                    let mut match_indices: Vec<usize> = Vec::new();
                    for (line_idx, line) in lines.iter().enumerate() {
                        if re.is_match(line) {
                            match_indices.push(line_idx);
                        }
                    }

                    if match_indices.is_empty() {
                        continue;
                    }

                    let path_str = path.display().to_string();
                    if seen_files.insert(path_str.clone()) {
                        files.push(path_str.clone());
                    }
                    *counts.entry(path_str.clone()).or_insert(0) += match_indices.len();

                    let mut selected_indices = BTreeSet::new();
                    if context > 0 {
                        for idx in match_indices {
                            let start = idx.saturating_sub(context);
                            let end = (idx + context).min(lines.len().saturating_sub(1));
                            for i in start..=end {
                                selected_indices.insert(i);
                            }
                        }
                    } else {
                        for idx in match_indices {
                            selected_indices.insert(idx);
                        }
                    }

                    for line_idx in selected_indices {
                        if matches.len() >= fetch_limit {
                            break;
                        }
                        matches.push(json!({
                            "file": path_str.clone(),
                            "line": line_idx + 1,
                            "content": lines[line_idx].chars().take(500).collect::<String>(),
                        }));
                    }
                }
            }
        }
    }

    async fn search_dir_names(glob_re: &Regex, dir: &std::path::Path, results: &mut Vec<Value>) {
        Self::search_dir_names_depth(glob_re, dir, results, 0).await;
    }

    async fn search_dir_names_depth(
        glob_re: &Regex,
        dir: &std::path::Path,
        results: &mut Vec<Value>,
        depth: u32,
    ) {
        if depth >= MAX_SEARCH_DEPTH {
            return;
        }

        let entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut entries = entries;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }

            if glob_re.is_match(name) {
                results.push(json!({
                    "path": path.display().to_string(),
                    "name": name,
                    "is_dir": path.is_dir(),
                }));
            }

            if path.is_dir() {
                Box::pin(Self::search_dir_names_depth(
                    glob_re,
                    &path,
                    results,
                    depth + 1,
                ))
                .await;
            }
        }
    }

    fn compile_glob_regex(pattern: &str) -> Result<Regex, ToolError> {
        // Simple glob matching: * matches any sequence, ? matches single char
        let re_pattern = pattern
            .replace('.', "\\.")
            .replace('*', ".*")
            .replace('?', ".");
        Regex::new(&format!("^{}$", re_pattern)).map_err(|e| {
            ToolError::ExecutionFailed(format!("Invalid glob pattern '{}': {}", pattern, e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_content_supports_offset_and_output_modes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("a.txt"), "alpha\nneedle one\nneedle two\nomega\n")
            .expect("write a.txt");
        std::fs::write(root.join("b.txt"), "needle three\nbeta\n").expect("write b.txt");

        let backend = LocalSearchBackend::new();

        let content = backend
            .search_content(
                "needle",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(2),
                Some(1),
                Some("content"),
                Some(0),
            )
            .await
            .expect("content search");
        let parsed: Value = serde_json::from_str(&content).expect("json");
        assert_eq!(parsed["total"], 3);
        assert_eq!(
            parsed["matches"].as_array().expect("matches array").len(),
            2
        );

        let files_only = backend
            .search_content(
                "needle",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(10),
                Some(0),
                Some("files_only"),
                Some(0),
            )
            .await
            .expect("files_only search");
        let parsed: Value = serde_json::from_str(&files_only).expect("json");
        assert_eq!(parsed["total"], 2);
        assert_eq!(parsed["files"].as_array().expect("files array").len(), 2);

        let counts = backend
            .search_content(
                "needle",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(10),
                Some(0),
                Some("count"),
                Some(0),
            )
            .await
            .expect("count search");
        let parsed: Value = serde_json::from_str(&counts).expect("json");
        assert_eq!(parsed["total"], 3);
    }

    #[tokio::test]
    async fn search_content_invalid_regex_surfaces_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("a.txt"), "needle\n").expect("write a.txt");

        let backend = LocalSearchBackend::new();
        let err = backend
            .search_content(
                "[",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(10),
                Some(0),
                Some("content"),
                Some(0),
            )
            .await
            .expect_err("invalid regex should fail before traversal");

        assert!(err.to_string().contains("Invalid regex pattern"));
    }

    #[tokio::test]
    async fn search_content_read_errors_keep_matches_and_do_not_pollute_modes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let good = root.join("good.txt");
        let bad = root.join("bad.txt");
        std::fs::write(&good, "needle in readable file\n").expect("write good");
        std::fs::write(&bad, b"\xff\xfe needle in non utf8 file").expect("write bad");

        let backend = LocalSearchBackend::new();

        let content = backend
            .search_content(
                "needle",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(10),
                Some(0),
                Some("content"),
                Some(0),
            )
            .await
            .expect("content search keeps readable matches");
        let parsed: Value = serde_json::from_str(&content).expect("json");
        let matches = parsed["matches"].as_array().expect("matches array");
        assert_eq!(parsed["total"], 1);
        assert_eq!(matches.len(), 1);
        assert!(matches[0]["file"]
            .as_str()
            .expect("match file")
            .ends_with("good.txt"));
        assert!(!content.contains("bad.txt"));

        let files_only = backend
            .search_content(
                "needle",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(10),
                Some(0),
                Some("files_only"),
                Some(0),
            )
            .await
            .expect("files_only search keeps readable matches");
        let parsed: Value = serde_json::from_str(&files_only).expect("json");
        let files = parsed["files"].as_array().expect("files array");
        assert_eq!(parsed["total"], 1);
        assert_eq!(files.len(), 1);
        assert!(files[0].as_str().expect("file").ends_with("good.txt"));
        assert!(!files_only.contains("bad.txt"));

        let counts = backend
            .search_content(
                "needle",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(10),
                Some(0),
                Some("count"),
                Some(0),
            )
            .await
            .expect("count search keeps readable matches");
        let parsed: Value = serde_json::from_str(&counts).expect("json");
        let counts = parsed["counts"].as_object().expect("counts object");
        assert_eq!(parsed["total"], 1);
        assert_eq!(counts.len(), 1);
        assert!(counts.keys().any(|path| path.ends_with("good.txt")));
        assert!(!counts.keys().any(|path| path.ends_with("bad.txt")));
    }

    #[tokio::test]
    async fn search_content_context_includes_surrounding_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("ctx.txt"), "line1\nline2\nneedle\nline4\nline5\n")
            .expect("write ctx.txt");

        let backend = LocalSearchBackend::new();
        let content = backend
            .search_content(
                "needle",
                root.to_str().expect("path str"),
                Some("*.txt"),
                Some(10),
                Some(0),
                Some("content"),
                Some(1),
            )
            .await
            .expect("context search");
        let parsed: Value = serde_json::from_str(&content).expect("json");
        let lines: Vec<i64> = parsed["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .filter_map(|m| m["line"].as_i64())
            .collect();
        assert!(lines.contains(&2));
        assert!(lines.contains(&3));
        assert!(lines.contains(&4));
    }

    #[tokio::test]
    async fn search_files_supports_offset_and_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("alpha.txt"), "a").expect("write alpha");
        std::fs::write(root.join("beta.txt"), "b").expect("write beta");
        std::fs::write(root.join("gamma.txt"), "c").expect("write gamma");

        let backend = LocalSearchBackend::new();
        let out = backend
            .search_files("*.txt", root.to_str().expect("path str"), Some(1), Some(1))
            .await
            .expect("search files");
        let parsed: Value = serde_json::from_str(&out).expect("json");
        assert_eq!(parsed["total"], 3);
        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["files"].as_array().expect("files array").len(), 1);
    }

    #[tokio::test]
    async fn patch_backend_uses_fuzzy_match_module() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("smart.rs");
        std::fs::write(&file, "let s = “hello”;\n").expect("write smart");

        let backend = LocalPatchBackend::new();
        let out = backend
            .patch_file(
                file.to_str().expect("path str"),
                "let s = \"hello\";",
                "let s = \"bye\";",
                false,
            )
            .await
            .expect("patch");
        let parsed: Value = serde_json::from_str(&out).expect("json");
        assert_eq!(parsed["replacements"], 1);
        assert_eq!(parsed["strategy"], "unicode_normalized");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read smart"),
            "let s = \"bye\";\n"
        );
    }
}
