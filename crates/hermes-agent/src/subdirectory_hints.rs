//! Progressive subdirectory hint discovery.
//!
//! Ported from Python `agent/subdirectory_hints.py`.
//!
//! As the agent navigates into subdirectories via tool calls, this module
//! discovers and loads project context files (AGENTS.md, CLAUDE.md, DESIGN.md,
//! .cursorrules) from those directories. Discovered hints are appended
//! to the tool result so the model gets relevant context at the moment
//! it starts working in a new area of the codebase.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::context_files::scan_context_content;

/// Context files to look for in subdirectories, in priority order.
const HINT_FILENAMES: &[&str] = &[
    "AGENTS.md",
    "agents.md",
    "CLAUDE.md",
    "claude.md",
    "DESIGN.md",
    "design.md",
    ".cursorrules",
];

/// Maximum chars per hint file to prevent context bloat.
const MAX_HINT_CHARS: usize = 8_000;

/// Tool argument keys that typically contain file paths.
const PATH_ARG_KEYS: &[&str] = &["path", "file_path", "workdir"];

/// Tools that take shell commands where we should extract paths.
const COMMAND_TOOLS: &[&str] = &["terminal"];

/// How many parent directories to walk up when looking for hints.
const MAX_ANCESTOR_WALK: usize = 5;

/// Track which directories the agent visits and load hints on first access.
pub struct SubdirectoryHintTracker {
    /// The project working directory.
    working_dir: PathBuf,
    /// Set of directories already loaded (to avoid re-scanning).
    loaded_dirs: HashSet<PathBuf>,
}

impl SubdirectoryHintTracker {
    /// Create a new tracker rooted at the given working directory.
    pub fn new(working_dir: &Path) -> Self {
        let wd = working_dir.to_path_buf();
        let mut loaded = HashSet::new();
        // Pre-mark the working dir as loaded (startup context handles it)
        loaded.insert(wd.clone());
        Self {
            working_dir: wd,
            loaded_dirs: loaded,
        }
    }

    /// Check tool call arguments for new directories and load any hint files.
    ///
    /// Returns formatted hint text to append to the tool result, or `None`.
    pub fn check_tool_call(
        &mut self,
        tool_name: &str,
        tool_args: &serde_json::Value,
    ) -> Option<String> {
        let dirs = self.extract_directories(tool_name, tool_args);
        if dirs.is_empty() {
            return None;
        }

        let mut all_hints = Vec::new();
        for dir in dirs {
            if let Some(hints) = self.load_hints_for_directory(&dir) {
                all_hints.push(hints);
            }
        }

        if all_hints.is_empty() {
            return None;
        }

        Some(format!("\n\n{}", all_hints.join("\n\n")))
    }

    /// Extract directory paths from tool call arguments.
    fn extract_directories(&self, tool_name: &str, args: &serde_json::Value) -> Vec<PathBuf> {
        let mut candidates = HashSet::new();

        // Direct path arguments
        for &key in PATH_ARG_KEYS {
            if let Some(val) = args.get(key).and_then(|v| v.as_str()) {
                if !val.trim().is_empty() {
                    self.add_path_candidate(val, &mut candidates);
                }
            }
        }

        // Shell commands — extract path-like tokens
        if COMMAND_TOOLS.contains(&tool_name) {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                self.extract_paths_from_command(cmd, &mut candidates);
            }
        }

        candidates.into_iter().collect()
    }

    /// Resolve a raw path and add its directory + ancestors to candidates.
    fn add_path_candidate(&self, raw_path: &str, candidates: &mut HashSet<PathBuf>) {
        let p = Path::new(raw_path);
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.working_dir.join(p)
        };

        // Use parent if it looks like a file path
        let dir = if resolved.extension().is_some() || (resolved.exists() && resolved.is_file()) {
            resolved.parent().map(|p| p.to_path_buf())
        } else {
            Some(resolved)
        };

        if let Some(mut current) = dir {
            for _ in 0..MAX_ANCESTOR_WALK {
                if self.loaded_dirs.contains(&current) {
                    break;
                }
                if current.is_dir() {
                    candidates.insert(current.clone());
                }
                match current.parent() {
                    Some(parent) if parent != current => {
                        current = parent.to_path_buf();
                    }
                    _ => break,
                }
            }
        }
    }

    /// Extract path-like tokens from a shell command string.
    fn extract_paths_from_command(&self, cmd: &str, candidates: &mut HashSet<PathBuf>) {
        for token in cmd.split_whitespace() {
            // Skip flags
            if token.starts_with('-') {
                continue;
            }
            // Must look like a path
            if !token.contains('/') && !token.contains('.') {
                continue;
            }
            // Skip URLs
            if token.starts_with("http://")
                || token.starts_with("https://")
                || token.starts_with("git@")
            {
                continue;
            }
            self.add_path_candidate(token, candidates);
        }
    }

    /// Load hint files from a directory. Returns formatted text or None.
    fn load_hints_for_directory(&mut self, directory: &Path) -> Option<String> {
        self.loaded_dirs.insert(directory.to_path_buf());

        for &filename in HINT_FILENAMES {
            let hint_path = directory.join(filename);
            if !hint_path.is_file() {
                continue;
            }

            match std::fs::read_to_string(&hint_path) {
                Ok(content) => {
                    let trimmed = content.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Security scan
                    let scanned = scan_context_content(trimmed, filename);

                    // Truncate if too long
                    let final_content = if scanned.len() > MAX_HINT_CHARS {
                        format!(
                            "{}\n\n[...truncated {}: {} chars total]",
                            &scanned[..MAX_HINT_CHARS],
                            filename,
                            scanned.len()
                        )
                    } else {
                        scanned
                    };

                    // Best-effort relative path
                    let rel_path = hint_path
                        .strip_prefix(&self.working_dir)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| hint_path.display().to_string());

                    debug!("Loaded subdirectory hints from {}", rel_path);

                    return Some(format!(
                        "[Subdirectory context discovered: {}]\n{}",
                        rel_path, final_content
                    ));
                }
                Err(e) => {
                    debug!("Could not read {}: {}", hint_path.display(), e);
                }
            }
        }

        None
    }
}

/// Scan the current working directory for common project markers and generate
/// a brief description of the project structure.
pub fn generate_project_hints(working_dir: &Path) -> String {
    let mut hints = Vec::new();

    let markers = [
        ("package.json", "Node.js/JavaScript project"),
        ("Cargo.toml", "Rust project"),
        ("pyproject.toml", "Python project (pyproject)"),
        ("setup.py", "Python project (setup.py)"),
        ("requirements.txt", "Python project (requirements)"),
        ("go.mod", "Go project"),
        ("pom.xml", "Java/Maven project"),
        ("build.gradle", "Java/Gradle project"),
        ("Gemfile", "Ruby project"),
        ("composer.json", "PHP/Composer project"),
        ("CMakeLists.txt", "C/C++ CMake project"),
        ("Makefile", "Project with Makefile"),
        ("Dockerfile", "Docker containerized"),
        ("docker-compose.yml", "Docker Compose setup"),
        (".git", "Git repository"),
        ("tsconfig.json", "TypeScript project"),
        (".env", "Environment variables configured"),
    ];

    for (marker, description) in &markers {
        if working_dir.join(marker).exists() {
            hints.push(*description);
        }
    }

    // Count subdirectories for structure overview
    if let Ok(entries) = std::fs::read_dir(working_dir) {
        let dirs: Vec<String> = entries
            .flatten()
            .filter(|e| {
                e.path().is_dir()
                    && !e.file_name().to_string_lossy().starts_with('.')
                    && e.file_name() != "node_modules"
                    && e.file_name() != "target"
                    && e.file_name() != "__pycache__"
                    && e.file_name() != ".git"
            })
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        if !dirs.is_empty() {
            let dir_list = if dirs.len() <= 10 {
                dirs.join(", ")
            } else {
                format!("{}, ... ({} total)", dirs[..10].join(", "), dirs.len())
            };
            let project_type = if hints.is_empty() {
                "unknown".to_string()
            } else {
                hints.join(", ")
            };
            return format!(
                "Working directory: {}\nProject type: {}\nSubdirectories: {}",
                working_dir.display(),
                project_type,
                dir_list
            );
        }
    }

    if hints.is_empty() {
        return format!("Working directory: {}", working_dir.display());
    }

    format!(
        "Working directory: {}\nProject type: {}",
        working_dir.display(),
        hints.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracker_new() {
        let tmp = tempfile::tempdir().unwrap();
        let tracker = SubdirectoryHintTracker::new(tmp.path());
        assert!(tracker.loaded_dirs.contains(tmp.path()));
    }

    #[test]
    fn test_tracker_check_no_hints() {
        let tmp = tempfile::tempdir().unwrap();
        let mut tracker = SubdirectoryHintTracker::new(tmp.path());

        let args = serde_json::json!({"path": "nonexistent/file.rs"});
        let result = tracker.check_tool_call("read_file", &args);
        assert!(result.is_none());
    }

    #[test]
    fn test_tracker_discovers_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("backend");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("AGENTS.md"), "Backend instructions").unwrap();

        let mut tracker = SubdirectoryHintTracker::new(tmp.path());
        let args = serde_json::json!({"path": "backend/main.rs"});
        let result = tracker.check_tool_call("read_file", &args);

        assert!(result.is_some());
        assert!(result.unwrap().contains("Backend instructions"));
    }

    #[test]
    fn test_tracker_no_duplicate_loading() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("src");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("AGENTS.md"), "Source instructions").unwrap();

        let mut tracker = SubdirectoryHintTracker::new(tmp.path());

        // First call should find hints
        let args = serde_json::json!({"path": "src/main.rs"});
        let r1 = tracker.check_tool_call("read_file", &args);
        assert!(r1.is_some());

        // Second call to same dir should not re-load
        let r2 = tracker.check_tool_call("read_file", &args);
        assert!(r2.is_none());
    }

    #[test]
    fn test_tracker_discovers_design_md() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("frontend");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("DESIGN.md"), "Design tokens and rules").unwrap();

        let mut tracker = SubdirectoryHintTracker::new(tmp.path());
        let args = serde_json::json!({"path": "frontend/app.tsx"});
        let result = tracker.check_tool_call("read_file", &args);

        assert!(result.is_some());
        assert!(result.unwrap().contains("Design tokens and rules"));
    }

    #[test]
    fn test_generate_project_hints_rust() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();

        let hints = generate_project_hints(tmp.path());
        assert!(hints.contains("Rust project"));
    }

    #[test]
    fn test_generate_project_hints_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let hints = generate_project_hints(tmp.path());
        assert!(hints.contains("Working directory"));
    }

    #[test]
    fn test_extract_paths_from_command() {
        let tmp = tempfile::tempdir().unwrap();
        // Create the src directory so it's recognized as a valid path
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();

        let tracker = SubdirectoryHintTracker::new(tmp.path());
        let args = serde_json::json!({"command": "cat src/main.rs"});
        let dirs = tracker.extract_directories("terminal", &args);
        // Should extract src directory from the command
        assert!(
            !dirs.is_empty(),
            "Expected directories from command path extraction"
        );
    }
}
