//! Real file tool backends: patch (fuzzy match) and search (regex/glob).

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::tools::file::{PatchBackend, SearchBackend};
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

    /// Find the best fuzzy match for `needle` in `haystack`.
    /// Returns (start_index, end_index) of the best match, or None.
    fn fuzzy_find(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        // Strategy 1: Exact match
        if let Some(pos) = haystack.find(needle) {
            return Some((pos, pos + needle.len()));
        }

        // Strategy 2: Line-trimmed match
        if let Some(result) = Self::strategy_line_trimmed(haystack, needle) {
            return Some(result);
        }

        // Strategy 3: Whitespace normalized
        if let Some(result) = Self::strategy_whitespace_normalized(haystack, needle) {
            return Some(result);
        }

        // Strategy 4: Indentation flexible
        if let Some(result) = Self::strategy_indentation_flexible(haystack, needle) {
            return Some(result);
        }

        // Strategy 5: Escape normalized
        if let Some(result) = Self::strategy_escape_normalized(haystack, needle) {
            return Some(result);
        }

        // Strategy 6: Trimmed boundary
        if let Some(result) = Self::strategy_trimmed_boundary(haystack, needle) {
            return Some(result);
        }

        // Strategy 7: Block anchor
        if let Some(result) = Self::strategy_block_anchor(haystack, needle) {
            return Some(result);
        }

        // Strategy 8: Context-aware
        Self::strategy_context_aware(haystack, needle)
    }

    /// Strategy 2: Match with line-by-line whitespace trimming.
    fn strategy_line_trimmed(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().map(|l| l.trim()).collect();
        let haystack_lines: Vec<&str> = haystack.lines().collect();

        if needle_lines.is_empty() {
            return None;
        }

        for i in 0..haystack_lines.len().saturating_sub(needle_lines.len() - 1) {
            let mut matches = true;
            for (j, nl) in needle_lines.iter().enumerate() {
                if i + j >= haystack_lines.len() || haystack_lines[i + j].trim() != *nl {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Some(Self::line_positions(
                    haystack,
                    &haystack_lines,
                    i,
                    i + needle_lines.len(),
                ));
            }
        }
        None
    }

    /// Strategy 3: Collapse multiple whitespace to single space.
    fn strategy_whitespace_normalized(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let norm_needle = Self::normalize_whitespace(needle);
        let norm_haystack = Self::normalize_whitespace(haystack);

        if let Some(pos) = norm_haystack.find(&norm_needle) {
            let start = Self::map_normalized_pos(haystack, &norm_haystack, pos);
            let end = Self::map_normalized_pos(haystack, &norm_haystack, pos + norm_needle.len());
            return Some((start, end));
        }
        None
    }

    /// Strategy 4: Ignore indentation differences entirely.
    fn strategy_indentation_flexible(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().map(|l| l.trim_start()).collect();
        let haystack_lines: Vec<&str> = haystack.lines().collect();

        if needle_lines.is_empty() {
            return None;
        }

        for i in 0..haystack_lines.len().saturating_sub(needle_lines.len() - 1) {
            let mut matches = true;
            for (j, nl) in needle_lines.iter().enumerate() {
                if i + j >= haystack_lines.len() || haystack_lines[i + j].trim_start() != *nl {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Some(Self::line_positions(
                    haystack,
                    &haystack_lines,
                    i,
                    i + needle_lines.len(),
                ));
            }
        }
        None
    }

    /// Strategy 5: Convert escape sequences to actual characters.
    fn strategy_escape_normalized(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let unescaped = needle
            .replace("\\n", "\n")
            .replace("\\t", "\t")
            .replace("\\r", "\r");

        if unescaped == needle {
            return None; // No escapes to convert
        }

        haystack
            .find(&unescaped)
            .map(|pos| (pos, pos + unescaped.len()))
    }

    /// Strategy 6: Trim whitespace from first and last lines only.
    fn strategy_trimmed_boundary(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let mut needle_lines: Vec<String> = needle.lines().map(|l| l.to_string()).collect();
        if needle_lines.is_empty() {
            return None;
        }

        needle_lines[0] = needle_lines[0].trim().to_string();
        if needle_lines.len() > 1 {
            let last = needle_lines.len() - 1;
            needle_lines[last] = needle_lines[last].trim().to_string();
        }

        let haystack_lines: Vec<&str> = haystack.lines().collect();
        let n = needle_lines.len();

        for i in 0..haystack_lines.len().saturating_sub(n - 1) {
            let mut check_lines: Vec<String> = haystack_lines[i..i + n]
                .iter()
                .map(|l| l.to_string())
                .collect();
            check_lines[0] = check_lines[0].trim().to_string();
            if check_lines.len() > 1 {
                let last = check_lines.len() - 1;
                check_lines[last] = check_lines[last].trim().to_string();
            }

            if check_lines == needle_lines {
                return Some(Self::line_positions(haystack, &haystack_lines, i, i + n));
            }
        }
        None
    }

    /// Strategy 7: Match by anchoring on first and last lines.
    fn strategy_block_anchor(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().collect();
        if needle_lines.len() < 2 {
            return None;
        }

        let first_line = needle_lines[0].trim();
        let last_line = needle_lines[needle_lines.len() - 1].trim();
        let haystack_lines: Vec<&str> = haystack.lines().collect();
        let n = needle_lines.len();

        let mut candidates = Vec::new();
        for i in 0..haystack_lines.len().saturating_sub(n - 1) {
            if haystack_lines[i].trim() == first_line
                && haystack_lines[i + n - 1].trim() == last_line
            {
                candidates.push(i);
            }
        }

        // Use lower threshold for unique matches
        let threshold = if candidates.len() == 1 { 0.10 } else { 0.30 };

        for i in candidates {
            let similarity = if n <= 2 {
                1.0
            } else {
                let content_middle: String = haystack_lines[i + 1..i + n - 1].join("\n");
                let pattern_middle: String = needle_lines[1..n - 1].join("\n");
                Self::sequence_similarity(&content_middle, &pattern_middle)
            };

            if similarity >= threshold {
                return Some(Self::line_positions(haystack, &haystack_lines, i, i + n));
            }
        }
        None
    }

    /// Strategy 8: Line-by-line similarity with 50% threshold.
    fn strategy_context_aware(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let needle_lines: Vec<&str> = needle.lines().collect();
        let haystack_lines: Vec<&str> = haystack.lines().collect();

        if needle_lines.is_empty() {
            return None;
        }

        let n = needle_lines.len();

        for i in 0..haystack_lines.len().saturating_sub(n - 1) {
            let block = &haystack_lines[i..i + n];
            let mut high_sim_count = 0;

            for (pl, cl) in needle_lines.iter().zip(block.iter()) {
                let sim = Self::sequence_similarity(pl.trim(), cl.trim());
                if sim >= 0.80 {
                    high_sim_count += 1;
                }
            }

            if high_sim_count >= (n as f64 * 0.5) as usize {
                return Some(Self::line_positions(haystack, &haystack_lines, i, i + n));
            }
        }
        None
    }

    fn normalize_whitespace(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn map_normalized_pos(original: &str, _normalized: &str, norm_pos: usize) -> usize {
        let mut orig_idx = 0;
        let mut norm_idx = 0;
        let orig_bytes = original.as_bytes();

        while norm_idx < norm_pos && orig_idx < orig_bytes.len() {
            if orig_bytes[orig_idx].is_ascii_whitespace() {
                orig_idx += 1;
                if norm_idx < norm_pos {
                    while orig_idx < orig_bytes.len() && orig_bytes[orig_idx].is_ascii_whitespace()
                    {
                        orig_idx += 1;
                    }
                    norm_idx += 1;
                }
            } else {
                orig_idx += 1;
                norm_idx += 1;
            }
        }

        orig_idx.min(original.len())
    }

    /// Calculate start and end byte positions from line indices.
    fn line_positions(
        content: &str,
        lines: &[&str],
        start_line: usize,
        end_line: usize,
    ) -> (usize, usize) {
        let start: usize = lines[..start_line].iter().map(|l| l.len() + 1).sum();
        let end: usize = lines[..end_line].iter().map(|l| l.len() + 1).sum();
        let end = (end.saturating_sub(1)).min(content.len());
        (start, end)
    }

    /// Simple sequence similarity ratio (0.0 to 1.0).
    fn sequence_similarity(a: &str, b: &str) -> f64 {
        if a.is_empty() && b.is_empty() {
            return 1.0;
        }
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }

        // Use longest common subsequence ratio
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let lcs_len = Self::lcs_length(&a_chars, &b_chars);
        (2.0 * lcs_len as f64) / (a_chars.len() + b_chars.len()) as f64
    }

    /// Longest common subsequence length.
    fn lcs_length(a: &[char], b: &[char]) -> usize {
        let m = a.len();
        let n = b.len();
        // Use two rows to save memory
        let mut prev = vec![0usize; n + 1];
        let mut curr = vec![0usize; n + 1];

        for i in 1..=m {
            for j in 1..=n {
                if a[i - 1] == b[j - 1] {
                    curr[j] = prev[j - 1] + 1;
                } else {
                    curr[j] = prev[j].max(curr[j - 1]);
                }
            }
            std::mem::swap(&mut prev, &mut curr);
            curr.iter_mut().for_each(|x| *x = 0);
        }
        *prev.iter().max().unwrap_or(&0)
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

        if replace_all {
            // Simple replace all occurrences
            if !content.contains(old_string) {
                return Err(ToolError::ExecutionFailed(format!(
                    "Could not find '{}' in '{}'",
                    &old_string[..old_string.len().min(100)],
                    path
                )));
            }
            let new_content = content.replace(old_string, new_string);
            let count = content.matches(old_string).count();
            tokio::fs::write(path, &new_content).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
            })?;
            return Ok(json!({"status": "ok", "replacements": count}).to_string());
        }

        // Single replacement with fuzzy matching
        match Self::fuzzy_find(&content, old_string) {
            Some((start, end)) => {
                let mut new_content = String::with_capacity(content.len());
                new_content.push_str(&content[..start]);
                new_content.push_str(new_string);
                new_content.push_str(&content[end..]);

                tokio::fs::write(path, &new_content).await.map_err(|e| {
                    ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
                })?;

                Ok(json!({"status": "ok", "replacements": 1}).to_string())
            }
            None => Err(ToolError::ExecutionFailed(format!(
                "Could not find a match for the specified text in '{}'",
                path
            ))),
        }
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
        let re = Regex::new(pattern)
            .map_err(|e| ToolError::InvalidParams(format!("Invalid regex pattern: {}", e)))?;

        let max = max_results.unwrap_or(50);
        let offset = offset.unwrap_or(0);
        let output_mode = output_mode.unwrap_or("content");
        let context = context.unwrap_or(0);
        let fetch_limit = if context > 0 {
            max.saturating_add(offset).saturating_add(200)
        } else {
            max.saturating_add(offset)
        };

        let mut matches: Vec<Value> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();

        let path = std::path::Path::new(path);
        if !path.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "Path '{}' does not exist",
                path.display()
            )));
        }

        Self::search_dir_content(
            &re,
            path,
            file_glob,
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
        let mut results: Vec<Value> = Vec::new();
        let base = std::path::Path::new(path);

        if !base.exists() {
            return Err(ToolError::ExecutionFailed(format!(
                "Path '{}' does not exist",
                base.display()
            )));
        }

        Self::search_dir_names(pattern, base, &mut results).await;

        let max = max_results.unwrap_or(50);
        let offset = offset.unwrap_or(0);
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
        file_glob: Option<&str>,
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
            file_glob,
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
        file_glob: Option<&str>,
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
                    file_glob,
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
                if let Some(glob) = file_glob {
                    if !Self::matches_glob(name, glob) {
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

    async fn search_dir_names(pattern: &str, dir: &std::path::Path, results: &mut Vec<Value>) {
        Self::search_dir_names_depth(pattern, dir, results, 0).await;
    }

    async fn search_dir_names_depth(
        pattern: &str,
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

            if Self::matches_glob(name, pattern) {
                results.push(json!({
                    "path": path.display().to_string(),
                    "name": name,
                    "is_dir": path.is_dir(),
                }));
            }

            if path.is_dir() {
                Box::pin(Self::search_dir_names_depth(
                    pattern,
                    &path,
                    results,
                    depth + 1,
                ))
                .await;
            }
        }
    }

    fn matches_glob(name: &str, pattern: &str) -> bool {
        // Simple glob matching: * matches any sequence, ? matches single char
        let re_pattern = pattern
            .replace('.', "\\.")
            .replace('*', ".*")
            .replace('?', ".");
        Regex::new(&format!("^{}$", re_pattern))
            .map(|re| re.is_match(name))
            .unwrap_or(false)
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
}
