//! V4A Patch Format Parser.
//!
//! Ported from Python `tools/patch_parser.py`.
//!
//! Parses the V4A patch format used by codex, cline, and other coding agents.
//!
//! V4A Format:
//! ```text
//! *** Begin Patch
//! *** Update File: path/to/file.py
//! @@ optional context hint @@
//!  context line (space prefix)
//! -removed line (minus prefix)
//! +added line (plus prefix)
//! *** Add File: path/to/new.py
//! +new file content
//! *** Delete File: path/to/old.py
//! *** Move File: old/path.py -> new/path.py
//! *** End Patch
//! ```

use regex::Regex;
use std::fmt;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Type of patch operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    Add,
    Update,
    Delete,
    Move,
}

impl fmt::Display for OperationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperationType::Add => write!(f, "add"),
            OperationType::Update => write!(f, "update"),
            OperationType::Delete => write!(f, "delete"),
            OperationType::Move => write!(f, "move"),
        }
    }
}

/// A single line in a patch hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct HunkLine {
    /// Prefix: ' ' (context), '-' (removed), '+' (added).
    pub prefix: char,
    /// Line content (without the prefix character).
    pub content: String,
}

/// A group of changes within a file, optionally with a context hint.
#[derive(Debug, Clone, Default)]
pub struct Hunk {
    /// Optional `@@` context hint for positioning.
    pub context_hint: Option<String>,
    /// Lines in this hunk.
    pub lines: Vec<HunkLine>,
}

/// A single operation in a V4A patch.
#[derive(Debug, Clone)]
pub struct PatchOperation {
    /// Type of operation.
    pub operation: OperationType,
    /// File path being operated on.
    pub file_path: String,
    /// New path (for move operations only).
    pub new_path: Option<String>,
    /// Hunks containing the actual changes (for update/add operations).
    pub hunks: Vec<Hunk>,
}

/// Result of applying V4A patch operations.
#[derive(Debug, Clone, Default)]
pub struct PatchResult {
    pub files_modified: Vec<String>,
    pub files_created: Vec<String>,
    pub files_deleted: Vec<String>,
    pub diffs: Vec<String>,
    pub errors: Vec<String>,
}

impl PatchResult {
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a V4A format patch string into a list of operations.
///
/// Returns `(operations, error)`. On success, error is `None`.
pub fn parse_v4a_patch(patch_content: &str) -> (Vec<PatchOperation>, Option<String>) {
    let lines: Vec<&str> = patch_content.split('\n').collect();

    // Find patch boundaries
    let mut start_idx: Option<usize> = None;
    let mut end_idx: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if line.contains("*** Begin Patch") || line.contains("***Begin Patch") {
            start_idx = Some(i);
        } else if line.contains("*** End Patch") || line.contains("***End Patch") {
            end_idx = Some(i);
            break;
        }
    }

    // Allow parsing without explicit begin marker
    let start = start_idx.map(|i| i + 1).unwrap_or(0);
    let end = end_idx.unwrap_or(lines.len());

    let update_re = Regex::new(r"\*\*\*\s*Update\s+File:\s*(.+)").unwrap();
    let add_re = Regex::new(r"\*\*\*\s*Add\s+File:\s*(.+)").unwrap();
    let delete_re = Regex::new(r"\*\*\*\s*Delete\s+File:\s*(.+)").unwrap();
    let move_re = Regex::new(r"\*\*\*\s*Move\s+File:\s*(.+?)\s*->\s*(.+)").unwrap();
    let hint_re = Regex::new(r"@@\s*(.+?)\s*@@").unwrap();

    let mut operations: Vec<PatchOperation> = Vec::new();
    let mut current_op: Option<PatchOperation> = None;
    let mut current_hunk: Option<Hunk> = None;

    let mut i = start;
    while i < end {
        let line = lines[i];

        if let Some(caps) = update_re.captures(line) {
            // Save previous operation
            finalize_op(&mut current_op, &mut current_hunk, &mut operations);
            current_op = Some(PatchOperation {
                operation: OperationType::Update,
                file_path: caps[1].trim().to_string(),
                new_path: None,
                hunks: Vec::new(),
            });
            current_hunk = None;
        } else if let Some(caps) = add_re.captures(line) {
            finalize_op(&mut current_op, &mut current_hunk, &mut operations);
            current_op = Some(PatchOperation {
                operation: OperationType::Add,
                file_path: caps[1].trim().to_string(),
                new_path: None,
                hunks: Vec::new(),
            });
            current_hunk = Some(Hunk::default());
        } else if let Some(caps) = delete_re.captures(line) {
            finalize_op(&mut current_op, &mut current_hunk, &mut operations);
            operations.push(PatchOperation {
                operation: OperationType::Delete,
                file_path: caps[1].trim().to_string(),
                new_path: None,
                hunks: Vec::new(),
            });
            current_op = None;
            current_hunk = None;
        } else if let Some(caps) = move_re.captures(line) {
            finalize_op(&mut current_op, &mut current_hunk, &mut operations);
            operations.push(PatchOperation {
                operation: OperationType::Move,
                file_path: caps[1].trim().to_string(),
                new_path: Some(caps[2].trim().to_string()),
                hunks: Vec::new(),
            });
            current_op = None;
            current_hunk = None;
        } else if line.starts_with("@@") {
            if current_op.is_some() {
                // Save current hunk if it has lines
                if let Some(ref mut op) = current_op {
                    if let Some(hunk) = current_hunk.take() {
                        if !hunk.lines.is_empty() {
                            op.hunks.push(hunk);
                        }
                    }
                }
                let hint = hint_re.captures(line).map(|c| c[1].to_string());
                current_hunk = Some(Hunk {
                    context_hint: hint,
                    lines: Vec::new(),
                });
            }
        } else if current_op.is_some() && !line.is_empty() {
            // Parse hunk line
            if current_hunk.is_none() {
                current_hunk = Some(Hunk::default());
            }
            if let Some(ref mut hunk) = current_hunk {
                if line.starts_with('+') {
                    hunk.lines.push(HunkLine { prefix: '+', content: line[1..].to_string() });
                } else if line.starts_with('-') {
                    hunk.lines.push(HunkLine { prefix: '-', content: line[1..].to_string() });
                } else if line.starts_with(' ') {
                    hunk.lines.push(HunkLine { prefix: ' ', content: line[1..].to_string() });
                } else if line.starts_with('\\') {
                    // "\ No newline at end of file" — skip
                } else {
                    // Treat as context line (implicit space prefix)
                    hunk.lines.push(HunkLine { prefix: ' ', content: line.to_string() });
                }
            }
        }

        i += 1;
    }

    // Finalize last operation
    finalize_op(&mut current_op, &mut current_hunk, &mut operations);

    (operations, None)
}

/// Helper to finalize the current operation and push it to the list.
fn finalize_op(
    current_op: &mut Option<PatchOperation>,
    current_hunk: &mut Option<Hunk>,
    operations: &mut Vec<PatchOperation>,
) {
    if let Some(ref mut op) = current_op {
        if let Some(hunk) = current_hunk.take() {
            if !hunk.lines.is_empty() {
                op.hunks.push(hunk);
            }
        }
    }
    if let Some(op) = current_op.take() {
        operations.push(op);
    }
}

/// Extract the "old" (search) and "new" (replace) text from a hunk.
///
/// Context lines (' ') appear in both old and new.
/// Removed lines ('-') appear only in old.
/// Added lines ('+') appear only in new.
pub fn hunk_to_search_replace(hunk: &Hunk) -> (String, String) {
    let mut search_lines = Vec::new();
    let mut replace_lines = Vec::new();

    for line in &hunk.lines {
        match line.prefix {
            ' ' => {
                search_lines.push(line.content.as_str());
                replace_lines.push(line.content.as_str());
            }
            '-' => {
                search_lines.push(line.content.as_str());
            }
            '+' => {
                replace_lines.push(line.content.as_str());
            }
            _ => {}
        }
    }

    (search_lines.join("\n"), replace_lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_update_file() {
        let patch = r#"*** Begin Patch
*** Update File: src/main.rs
@@ fn main @@
 fn main() {
-    println!("old");
+    println!("new");
 }
*** End Patch"#;

        let (ops, err) = parse_v4a_patch(patch);
        assert!(err.is_none());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OperationType::Update);
        assert_eq!(ops[0].file_path, "src/main.rs");
        assert_eq!(ops[0].hunks.len(), 1);
        assert_eq!(ops[0].hunks[0].context_hint, Some("fn main".to_string()));

        let (search, replace) = hunk_to_search_replace(&ops[0].hunks[0]);
        assert!(search.contains("println!(\"old\")"));
        assert!(replace.contains("println!(\"new\")"));
    }

    #[test]
    fn test_parse_add_file() {
        let patch = r#"*** Begin Patch
*** Add File: new_file.txt
+line 1
+line 2
*** End Patch"#;

        let (ops, err) = parse_v4a_patch(patch);
        assert!(err.is_none());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OperationType::Add);
        assert_eq!(ops[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn test_parse_delete_file() {
        let patch = "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch";
        let (ops, _) = parse_v4a_patch(patch);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OperationType::Delete);
    }

    #[test]
    fn test_parse_move_file() {
        let patch = "*** Begin Patch\n*** Move File: old.rs -> new.rs\n*** End Patch";
        let (ops, _) = parse_v4a_patch(patch);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OperationType::Move);
        assert_eq!(ops[0].file_path, "old.rs");
        assert_eq!(ops[0].new_path, Some("new.rs".to_string()));
    }

    #[test]
    fn test_parse_multiple_operations() {
        let patch = r#"*** Begin Patch
*** Update File: a.rs
 fn a() {
-    old();
+    new();
 }
*** Add File: b.rs
+fn b() {}
*** Delete File: c.rs
*** End Patch"#;

        let (ops, _) = parse_v4a_patch(patch);
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].operation, OperationType::Update);
        assert_eq!(ops[1].operation, OperationType::Add);
        assert_eq!(ops[2].operation, OperationType::Delete);
    }

    #[test]
    fn test_parse_without_begin_end() {
        let patch = "*** Update File: test.rs\n fn test() {\n-    old();\n+    new();\n }";
        let (ops, _) = parse_v4a_patch(patch);
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn test_hunk_to_search_replace() {
        let hunk = Hunk {
            context_hint: None,
            lines: vec![
                HunkLine { prefix: ' ', content: "fn test() {".to_string() },
                HunkLine { prefix: '-', content: "    old();".to_string() },
                HunkLine { prefix: '+', content: "    new();".to_string() },
                HunkLine { prefix: ' ', content: "}".to_string() },
            ],
        };
        let (search, replace) = hunk_to_search_replace(&hunk);
        assert_eq!(search, "fn test() {\n    old();\n}");
        assert_eq!(replace, "fn test() {\n    new();\n}");
    }
}
