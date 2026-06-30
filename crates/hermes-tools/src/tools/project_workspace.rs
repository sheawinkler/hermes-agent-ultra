//! Read-only project/workspace inspection tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use hermes_core::{
    subprocess::CommandNoWindowExt, tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema,
};
use indexmap::IndexMap;
use serde_json::{json, Value};

const PROJECT_FACTS_TOOL: &str = "project_facts";
const PROJECT_TREE_TOOL: &str = "project_tree";
const MAX_FACT_FILE_BYTES: u64 = 256 * 1024;
const DEFAULT_TREE_DEPTH: usize = 3;
const DEFAULT_TREE_LIMIT: usize = 200;
const MAX_TREE_DEPTH: usize = 8;
const MAX_TREE_LIMIT: usize = 1_000;

const PROJECT_MARKERS: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "package.json",
    "tsconfig.json",
    "deno.json",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "pubspec.yaml",
    "CMakeLists.txt",
    "Makefile",
    "Dockerfile",
    "AGENTS.md",
    "CLAUDE.md",
    ".cursorrules",
];
const CONTEXT_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", ".cursorrules"];
const VERIFY_TARGETS: &[&str] = &[
    "test",
    "tests",
    "lint",
    "typecheck",
    "check",
    "build",
    "fmt",
    "format",
];
const DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".cache",
];

pub struct ProjectFactsHandler;
pub struct ProjectTreeHandler;

#[async_trait]
impl ToolHandler for ProjectFactsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let cwd = optional_path(&params, "cwd");
        Ok(project_facts_snapshot(cwd.as_deref()).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "cwd".into(),
            json!({
                "type": "string",
                "description": "Optional current working directory. Defaults to process cwd."
            }),
        );
        tool_schema(
            PROJECT_FACTS_TOOL,
            "Return read-only project/workspace facts: root, manifests, package managers, verify commands, context files, and git metadata.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[async_trait]
impl ToolHandler for ProjectTreeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let cwd = optional_path(&params, "cwd");
        let max_depth = params
            .get("max_depth")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(DEFAULT_TREE_DEPTH);
        let max_entries = params
            .get("max_entries")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(DEFAULT_TREE_LIMIT);
        let include_hidden = params
            .get("include_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(
            project_tree_snapshot(cwd.as_deref(), max_depth, max_entries, include_hidden)
                .to_string(),
        )
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "cwd".into(),
            json!({"type": "string", "description": "Optional cwd inside the project. Defaults to process cwd."}),
        );
        props.insert(
            "max_depth".into(),
            json!({"type": "integer", "description": "Maximum tree depth from project root. Defaults to 3, capped at 8.", "default": DEFAULT_TREE_DEPTH}),
        );
        props.insert(
            "max_entries".into(),
            json!({"type": "integer", "description": "Maximum entries to return. Defaults to 200, capped at 1000.", "default": DEFAULT_TREE_LIMIT}),
        );
        props.insert(
            "include_hidden".into(),
            json!({"type": "boolean", "description": "Include dotfiles except .git. Defaults to false.", "default": false}),
        );
        tool_schema(
            PROJECT_TREE_TOOL,
            "Return a bounded deterministic project tree rooted at the detected workspace root. Read-only; skips heavy dependency/build directories by default.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn project_facts_snapshot(cwd: Option<&Path>) -> Value {
    let requested_cwd = resolve_cwd(cwd);
    let Some(root) = workspace_root(&requested_cwd) else {
        return json!({
            "status": "not_applicable",
            "reason": "workspace_root_not_detected",
            "cwd": requested_cwd.display().to_string(),
        });
    };
    let manifests = project_manifest_names(&root);
    json!({
        "status": "ok",
        "root": root.display().to_string(),
        "cwd": requested_cwd.display().to_string(),
        "manifests": manifests,
        "packageManagers": package_managers(&root),
        "verifyCommands": verify_commands(&root),
        "contextFiles": context_files(&root),
        "git": git_snapshot(&root),
    })
}

pub fn project_tree_snapshot(
    cwd: Option<&Path>,
    max_depth: usize,
    max_entries: usize,
    include_hidden: bool,
) -> Value {
    let requested_cwd = resolve_cwd(cwd);
    let Some(root) = workspace_root(&requested_cwd) else {
        return json!({
            "status": "not_applicable",
            "reason": "workspace_root_not_detected",
            "cwd": requested_cwd.display().to_string(),
            "entries": [],
            "truncated": false,
        });
    };
    let max_depth = max_depth.clamp(1, MAX_TREE_DEPTH);
    let max_entries = max_entries.clamp(1, MAX_TREE_LIMIT);
    let collect_limit = max_entries.saturating_add(1);
    let mut entries = Vec::new();
    collect_tree(
        &root,
        &root,
        0,
        max_depth,
        collect_limit,
        include_hidden,
        &mut entries,
    );
    let truncated = entries.len() > max_entries;
    entries.truncate(max_entries);
    json!({
        "status": "ok",
        "root": root.display().to_string(),
        "cwd": requested_cwd.display().to_string(),
        "maxDepth": max_depth,
        "maxEntries": max_entries,
        "includeHidden": include_hidden,
        "truncated": truncated,
        "entries": entries,
    })
}

fn optional_path(params: &Value, key: &str) -> Option<PathBuf> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn resolve_cwd(cwd: Option<&Path>) -> PathBuf {
    let raw = cwd
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("TERMINAL_CWD").map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    raw.canonicalize().unwrap_or(raw)
}

fn workspace_root(cwd: &Path) -> Option<PathBuf> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    marker_root(&cwd).or_else(|| git_root(&cwd))
}

fn ancestors_nearest_first(start: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    std::iter::once(start.to_path_buf()).chain(start.ancestors().skip(1).map(Path::to_path_buf))
}

fn marker_root(cwd: &Path) -> Option<PathBuf> {
    ancestors_nearest_first(cwd).take(12).find(|parent| {
        PROJECT_MARKERS
            .iter()
            .any(|marker| parent.join(marker).exists())
    })
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    ancestors_nearest_first(cwd)
        .take(12)
        .find(|parent| parent.join(".git").exists())
}

fn project_manifest_names(root: &Path) -> Vec<String> {
    PROJECT_MARKERS
        .iter()
        .copied()
        .filter(|marker| !CONTEXT_FILES.contains(marker))
        .filter(|marker| root.join(marker).exists())
        .map(str::to_string)
        .collect()
}

fn context_files(root: &Path) -> Vec<String> {
    CONTEXT_FILES
        .iter()
        .copied()
        .filter(|name| root.join(name).is_file())
        .map(str::to_string)
        .collect()
}

fn package_managers(root: &Path) -> Vec<String> {
    let mut managers = Vec::new();
    if let Some(pm) = js_package_manager(root) {
        managers.push(pm.to_string());
    }
    if let Some(pm) = python_package_manager(root) {
        managers.push(pm.to_string());
    }
    dedup_truncate(managers)
}

fn js_package_manager(root: &Path) -> Option<&'static str> {
    [
        ("pnpm-lock.yaml", "pnpm"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
        ("yarn.lock", "yarn"),
        ("package-lock.json", "npm"),
    ]
    .iter()
    .find_map(|(file, manager)| root.join(file).exists().then_some(*manager))
}

fn python_package_manager(root: &Path) -> Option<&'static str> {
    [
        ("uv.lock", "uv"),
        ("poetry.lock", "poetry"),
        ("Pipfile.lock", "pipenv"),
    ]
    .iter()
    .find_map(|(file, manager)| root.join(file).exists().then_some(*manager))
}

fn verify_commands(root: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    append_package_json_verify(root, &mut commands);
    append_makefile_verify(root, &mut commands);
    if root.join("scripts").join("run_tests.sh").is_file() {
        commands.push("scripts/run_tests.sh".to_string());
    }
    if root.join("Cargo.toml").is_file() {
        commands.push("cargo test".to_string());
    }
    if root.join("go.mod").is_file() {
        commands.push("go test ./...".to_string());
    }
    if has_pytest_config(root) {
        commands.push("pytest".to_string());
    }
    dedup_truncate(commands)
}

fn append_package_json_verify(root: &Path, commands: &mut Vec<String>) {
    let path = root.join("package.json");
    if !small_file(&path) {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let Some(scripts) = value.get("scripts").and_then(Value::as_object) else {
        return;
    };
    let manager = js_package_manager(root).unwrap_or("npm");
    for target in VERIFY_TARGETS {
        if scripts.contains_key(*target) {
            commands.push(format!("{manager} run {target}"));
        }
    }
}

fn append_makefile_verify(root: &Path, commands: &mut Vec<String>) {
    let path = root.join("Makefile");
    if !small_file(&path) {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let targets = VERIFY_TARGETS
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for line in raw.lines() {
        let Some((name, _)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty()
            && !name.contains(char::is_whitespace)
            && !name.starts_with('.')
            && targets.contains(name)
        {
            commands.push(format!("make {name}"));
        }
    }
}

fn has_pytest_config(root: &Path) -> bool {
    if root.join("pytest.ini").is_file() || root.join("tox.ini").is_file() {
        return true;
    }
    let pyproject = root.join("pyproject.toml");
    if !small_file(&pyproject) {
        return false;
    }
    std::fs::read_to_string(pyproject)
        .map(|raw| raw.contains("[tool.pytest"))
        .unwrap_or(false)
}

fn small_file(path: &Path) -> bool {
    path.metadata()
        .map(|meta| meta.is_file() && meta.len() <= MAX_FACT_FILE_BYTES)
        .unwrap_or(false)
}

fn dedup_truncate(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeMap::new();
    for value in values {
        seen.entry(value).or_insert(());
    }
    seen.into_keys().take(8).collect()
}

fn git_snapshot(root: &Path) -> Value {
    let Some(git_root) = git_root(root) else {
        return json!({"present": false});
    };
    let branch = git_output(&git_root, &["branch", "--show-current"]);
    let head = git_output(&git_root, &["rev-parse", "--short", "HEAD"]);
    let status = git_output(&git_root, &["status", "--short"]);
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    if let Some(status) = status.as_deref() {
        for line in status.lines() {
            let code = line.get(..2).unwrap_or(line).trim();
            let key = if line.starts_with("??") {
                "untracked"
            } else if code == "UU" || code == "AA" || code == "DD" {
                "conflicts"
            } else if code.chars().next().is_some_and(|c| c != ' ') {
                "staged"
            } else {
                "modified"
            };
            *counts.entry(key).or_default() += 1;
        }
    }
    json!({
        "present": true,
        "root": git_root.display().to_string(),
        "branch": branch,
        "head": head,
        "dirty": !counts.is_empty(),
        "statusCounts": counts,
    })
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .suppress_windows_console()
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn ignored_tree_entry(name: &str, include_hidden: bool) -> bool {
    if name == ".git" {
        return true;
    }
    if !include_hidden && name.starts_with('.') {
        return true;
    }
    DEFAULT_IGNORED_DIRS.contains(&name)
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    max_entries: usize,
    include_hidden: bool,
    entries: &mut Vec<Value>,
) {
    if depth >= max_depth || entries.len() >= max_entries {
        return;
    }
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children = read_dir.flatten().collect::<Vec<_>>();
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        if entries.len() >= max_entries {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if ignored_tree_entry(&name, include_hidden) {
            continue;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let meta = entry.metadata().ok();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path.as_path())
            .display()
            .to_string();
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        entries.push(json!({
            "path": rel,
            "name": name,
            "kind": kind,
            "depth": depth + 1,
            "sizeBytes": meta.filter(|m| m.is_file()).map(|m| m.len()),
        }));
        if file_type.is_dir() {
            collect_tree(
                root,
                &path,
                depth + 1,
                max_depth,
                max_entries,
                include_hidden,
                entries,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_facts_reports_git_and_verify_commands() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='project-tool'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "# rules").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "user.email", "t@example.com"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "user.name", "Tester"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "Cargo.toml", "AGENTS.md"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "-qm", "init"])
            .status()
            .unwrap();

        let facts = project_facts_snapshot(Some(tmp.path()));

        assert_eq!(facts["status"], "ok");
        assert!(facts["manifests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "Cargo.toml"));
        assert!(facts["verifyCommands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "cargo test"));
        assert_eq!(facts["contextFiles"], json!(["AGENTS.md"]));
        assert_eq!(facts["git"]["present"], true);
        assert_eq!(facts["git"]["dirty"], false);
    }

    #[test]
    fn project_tree_is_bounded_and_skips_heavy_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("target/debug")).unwrap();
        std::fs::write(tmp.path().join("target/debug/big"), "ignored").unwrap();

        let tree = project_tree_snapshot(Some(tmp.path()), 4, 10, false);
        let paths = tree["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["path"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();

        assert_eq!(tree["status"], "ok");
        assert!(paths.contains(&"Cargo.toml".to_string()));
        assert!(paths.contains(&"src".to_string()));
        assert!(paths.contains(&"src/lib.rs".to_string()));
        assert!(!paths.iter().any(|path| path.starts_with("target")));
    }
}
