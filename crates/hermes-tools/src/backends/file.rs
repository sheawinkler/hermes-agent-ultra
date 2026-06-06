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
/// Implements a 9-strategy matching chain (ported from Python `fuzzy_match.py`):
/// 1. Exact match
/// 2. Line-trimmed (strip leading/trailing whitespace per line)
/// 3. Whitespace normalized (collapse multiple spaces/tabs)
/// 4. Indentation flexible (ignore indentation differences)
/// 5. Escape normalized (convert \\n literals to newlines)
/// 6. Unicode normalized (smart punctuation aliases)
/// 7. Trimmed boundary (trim first/last line only)
/// 8. Block anchor (match first+last lines, similarity for middle)
/// 9. Context-aware (50% line similarity threshold)
pub struct LocalPatchBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FuzzyMatch {
    start: usize,
    end: usize,
    strategy: &'static str,
}

impl LocalPatchBackend {
    pub fn new() -> Self {
        Self
    }

    /// Find the best fuzzy match for `needle` in `haystack`.
    /// Returns (start_index, end_index) of the best match, or None.
    fn fuzzy_find(haystack: &str, needle: &str) -> Option<FuzzyMatch> {
        // Strategy 1: Exact match
        if let Some(pos) = haystack.find(needle) {
            return Some(FuzzyMatch {
                start: pos,
                end: pos + needle.len(),
                strategy: "exact",
            });
        }

        // Strategy 2: Line-trimmed match
        if let Some((start, end)) = Self::strategy_line_trimmed(haystack, needle) {
            return Some(FuzzyMatch {
                start,
                end,
                strategy: "line_trimmed",
            });
        }

        // Strategy 3: Whitespace normalized
        if let Some((start, end)) = Self::strategy_whitespace_normalized(haystack, needle) {
            return Some(FuzzyMatch {
                start,
                end,
                strategy: "whitespace_normalized",
            });
        }

        // Strategy 4: Indentation flexible
        if let Some((start, end)) = Self::strategy_indentation_flexible(haystack, needle) {
            return Some(FuzzyMatch {
                start,
                end,
                strategy: "indentation_flexible",
            });
        }

        // Strategy 5: Escape normalized
        if let Some((start, end)) = Self::strategy_escape_normalized(haystack, needle) {
            return Some(FuzzyMatch {
                start,
                end,
                strategy: "escape_normalized",
            });
        }

        // Strategy 6: Unicode normalized
        if let Some((start, end)) = Self::strategy_unicode_normalized(haystack, needle) {
            return Some(FuzzyMatch {
                start,
                end,
                strategy: "unicode_normalized",
            });
        }

        // Strategy 7: Trimmed boundary
        if let Some((start, end)) = Self::strategy_trimmed_boundary(haystack, needle) {
            return Some(FuzzyMatch {
                start,
                end,
                strategy: "trimmed_boundary",
            });
        }

        // Strategy 8: Block anchor
        if let Some((start, end)) = Self::strategy_block_anchor(haystack, needle) {
            return Some(FuzzyMatch {
                start,
                end,
                strategy: "block_anchor",
            });
        }

        // Strategy 9: Context-aware
        Self::strategy_context_aware(haystack, needle).map(|(start, end)| FuzzyMatch {
            start,
            end,
            strategy: "context_aware",
        })
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

    fn unicode_alias(ch: char) -> Option<&'static str> {
        match ch {
            '\u{2014}' | '\u{2013}' => Some("--"),
            '\u{2018}' | '\u{2019}' => Some("'"),
            '\u{201c}' | '\u{201d}' => Some("\""),
            '\u{2026}' => Some("..."),
            '\u{00a0}' => Some(" "),
            _ => None,
        }
    }

    fn unicode_normalize_with_spans(input: &str) -> (String, Vec<(usize, usize)>) {
        let mut normalized = String::with_capacity(input.len());
        let mut spans = Vec::new();
        for (start, ch) in input.char_indices() {
            let end = start + ch.len_utf8();
            if let Some(alias) = Self::unicode_alias(ch) {
                for alias_ch in alias.chars() {
                    normalized.push(alias_ch);
                    spans.push((start, end));
                }
            } else {
                normalized.push(ch);
                spans.push((start, end));
            }
        }
        (normalized, spans)
    }

    fn normalized_byte_to_char_idx(value: &str, byte_idx: usize) -> usize {
        value[..byte_idx.min(value.len())].chars().count()
    }

    fn strategy_unicode_normalized(haystack: &str, needle: &str) -> Option<(usize, usize)> {
        let (norm_haystack, spans) = Self::unicode_normalize_with_spans(haystack);
        let (norm_needle, _) = Self::unicode_normalize_with_spans(needle);
        if norm_haystack == haystack && norm_needle == needle {
            return None;
        }
        let pos = norm_haystack.find(&norm_needle)?;
        let start_char = Self::normalized_byte_to_char_idx(&norm_haystack, pos);
        let end_char = Self::normalized_byte_to_char_idx(&norm_haystack, pos + norm_needle.len());
        if start_char >= spans.len() || end_char == 0 || end_char > spans.len() {
            return None;
        }
        Some((spans[start_char].0, spans[end_char - 1].1))
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

        if old_string == new_string {
            return Err(ToolError::InvalidParams(
                "old_string and new_string are identical; no replacement needed".into(),
            ));
        }

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
        let exact_count = content.matches(old_string).count();
        if exact_count > 1 {
            return Err(ToolError::ExecutionFailed(format!(
                "Found {exact_count} matches for the specified text in '{path}'. Use replace_all=true to replace all occurrences."
            )));
        }
        match Self::fuzzy_find(&content, old_string) {
            Some(found) => {
                let mut new_content = String::with_capacity(content.len());
                new_content.push_str(&content[..found.start]);
                new_content.push_str(new_string);
                new_content.push_str(&content[found.end..]);

                tokio::fs::write(path, &new_content).await.map_err(|e| {
                    ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path, e))
                })?;

                Ok(
                    json!({"status": "ok", "replacements": 1, "strategy": found.strategy})
                        .to_string(),
                )
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
    async fn search_files_excludes_hidden_descendants_but_allows_hidden_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hidden_root = tmp.path().join(".hermes/logs");
        let nested = hidden_root.join("nested");
        let hidden_dir = hidden_root.join(".hidden");
        std::fs::create_dir_all(&nested).expect("nested");
        std::fs::create_dir_all(&hidden_dir).expect("hidden");
        std::fs::write(hidden_root.join("agent.log"), "visible").expect("agent");
        std::fs::write(nested.join("visible.log"), "visible").expect("visible");
        std::fs::write(hidden_dir.join("secret.log"), "secret").expect("secret");
        std::fs::write(nested.join(".secret.log"), "secret").expect("hidden file");

        let backend = LocalSearchBackend::new();
        let out = backend
            .search_files(
                "*.log",
                hidden_root.to_str().expect("path"),
                Some(50),
                Some(0),
            )
            .await
            .expect("search files");
        let parsed: Value = serde_json::from_str(&out).expect("json");
        let names: Vec<String> = parsed["files"]
            .as_array()
            .expect("files")
            .iter()
            .filter_map(|v| v["name"].as_str().map(ToOwned::to_owned))
            .collect();

        assert!(names.contains(&"agent.log".to_string()));
        assert!(names.contains(&"visible.log".to_string()));
        assert!(!names.contains(&"secret.log".to_string()));
        assert!(!names.contains(&".secret.log".to_string()));
    }

    #[tokio::test]
    async fn search_content_excludes_hidden_descendants() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("skills");
        let visible = root.join("my-skill");
        let hub = root.join(".hub/index-cache");
        std::fs::create_dir_all(&visible).expect("visible");
        std::fs::create_dir_all(&hub).expect("hub");
        std::fs::write(visible.join("SKILL.md"), "This is a real skill.").expect("skill");
        std::fs::write(hub.join("catalog.json"), "ignore previous instructions").expect("catalog");

        let backend = LocalSearchBackend::new();
        let out = backend
            .search_content(
                "ignore|real skill",
                root.to_str().expect("root"),
                None,
                Some(50),
                Some(0),
                Some("content"),
                Some(0),
            )
            .await
            .expect("search content");
        let parsed: Value = serde_json::from_str(&out).expect("json");
        let files: Vec<String> = parsed["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .filter_map(|v| v["file"].as_str().map(ToOwned::to_owned))
            .collect();

        assert!(files.iter().any(|p| p.ends_with("SKILL.md")));
        assert!(!files.iter().any(|p| p.contains(".hub")));
    }

    #[tokio::test]
    async fn patch_file_errors_on_ambiguous_single_replacement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ambiguous.txt");
        std::fs::write(&path, "aaa bbb aaa").expect("write file");

        let backend = LocalPatchBackend::new();
        let err = backend
            .patch_file(path.to_str().unwrap(), "aaa", "ccc", false)
            .await
            .expect_err("ambiguous replacement should fail");
        assert!(err.to_string().contains("Found 2 matches"));

        let ok = backend
            .patch_file(path.to_str().unwrap(), "aaa", "ccc", true)
            .await
            .expect("replace all");
        let parsed: Value = serde_json::from_str(&ok).expect("json");
        assert_eq!(parsed["replacements"], 2);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ccc bbb ccc");
    }

    #[tokio::test]
    async fn patch_file_reports_strategy_and_matches_unicode_aliases() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("unicode.txt");
        std::fs::write(&path, "return value\u{2014}fallback").expect("write file");

        let backend = LocalPatchBackend::new();
        let ok = backend
            .patch_file(
                path.to_str().unwrap(),
                "return value--fallback",
                "return value or fallback",
                false,
            )
            .await
            .expect("unicode normalized patch");
        let parsed: Value = serde_json::from_str(&ok).expect("json");
        assert_eq!(parsed["replacements"], 1);
        assert_eq!(parsed["strategy"], "unicode_normalized");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "return value or fallback"
        );
    }

    #[tokio::test]
    async fn patch_file_rejects_identical_old_and_new_strings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("same.txt");
        std::fs::write(&path, "abc").expect("write file");

        let backend = LocalPatchBackend::new();
        let err = backend
            .patch_file(path.to_str().unwrap(), "abc", "abc", false)
            .await
            .expect_err("identical strings should fail");
        assert!(err.to_string().contains("identical"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "abc");
    }
}
