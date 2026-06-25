//! Coding-context posture detection and prompt blocks.
//!
//! This mirrors upstream's `agent.coding_context` contract in the Rust runtime:
//! interactive coding surfaces in a code workspace get a coding operating brief
//! plus a frozen workspace snapshot. `focus` mode additionally lets callers
//! collapse tools to the coding toolset; `auto` and `on` stay prompt-only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub const CODING_TOOLSET: &str = "coding";

const INTERACTIVE_CODING_PLATFORMS: &[&str] = &["", "cli", "tui", "acp", "desktop", "local"];
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
const MAX_VERIFY_COMMANDS: usize = 8;
const MAX_FACT_FILE_BYTES: u64 = 256 * 1024;

const NON_CODING_SKILL_CATEGORIES: &[&str] = &[
    "apple",
    "communication",
    "cooking",
    "creative",
    "email",
    "finance",
    "gaming",
    "gifs",
    "health",
    "media",
    "music",
    "note-taking",
    "productivity",
    "shopping",
    "smart-home",
    "social-media",
    "travel",
    "yuanbao",
];

const CODING_AGENT_GUIDANCE: &str =
    "You are a coding agent pairing with the user inside their codebase. Operate like a careful senior engineer.\n\
\n\
Gather context first:\n\
- Read the relevant files and locate code before changing anything. Trace a symbol to its definition and usages rather than guessing its shape.\n\
- Batch independent reads/searches when they do not depend on each other.\n\
- If ContextLattice tools are available, use them as the project memory and retrieval backbone before redoing broad discovery.\n\
- Never invent files, symbols, APIs, imports, or dependencies. Check manifests and nearby imports before using a library.\n\
\n\
Make changes through the tools, not the chat:\n\
- Edit with patch/write_file. Do not print code blocks to the user as a substitute for applying the change.\n\
- Match the project's existing style and instructions. Touch only what the task needs; avoid drive-by refactors, renames, or formatting churn.\n\
- If a patch fails, re-read the exact file contents before retrying. If the same region fails twice, rewrite the enclosing function or file with write_file instead of attempting a third stale patch.\n\
\n\
Verify, and know when to stop:\n\
- Use the terminal for git, builds, tests, and inspection. Run the relevant tests, linter, or build before claiming the work is done.\n\
- Fix root causes and sibling call paths for the same bug class, not only the reported site.\n\
- When fixing linter/type errors on a file, stop after about three attempts on the same file and ask the user rather than looping.\n\
- Track multi-step work with todo. Reference code as path:line instead of pasting whole files.\n\
\n\
Respect the user's repo: do not commit, push, or rewrite history unless asked, and never read, print, or commit secrets. The Workspace block below is a session-start snapshot; re-run git status/git branch before relying on it. Be concise: lead with the change or answer, not a preamble.";

const EDIT_FORMAT_PATCH: &str =
    "- Edit format: author new files with write_file; for edits to existing code prefer patch with mode='patch' (V4A multi-file diff) for structured or multi-file changes. Use mode='replace' for a single small swap.";
const EDIT_FORMAT_REPLACE: &str =
    "- Edit format: author new files with write_file; for edits to existing code prefer patch in mode='replace' by matching a unique snippet and swapping it. Reach for mode='patch' (V4A) only when an edit genuinely spans several files at once.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingContextMode {
    Auto,
    Focus,
    On,
    Off,
}

impl CodingContextMode {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
            "focus" | "strict" | "lean" => Self::Focus,
            "on" | "true" | "yes" | "1" | "always" => Self::On,
            "off" | "false" | "no" | "0" | "never" => Self::Off,
            _ => Self::Auto,
        }
    }

    pub fn is_focus(self) -> bool {
        self == Self::Focus
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextProfileKind {
    General,
    Coding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMode {
    pub profile: ContextProfileKind,
    pub mode: CodingContextMode,
    pub workspace_root: Option<PathBuf>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFacts {
    pub root: String,
    pub manifests: Vec<String>,
    pub package_managers: Vec<String>,
    pub verify_commands: Vec<String>,
    pub context_files: Vec<String>,
}

impl RuntimeMode {
    pub fn is_coding(&self) -> bool {
        self.profile == ContextProfileKind::Coding
    }

    pub fn toolset_selection(&self) -> Option<&'static str> {
        (self.is_coding() && self.mode.is_focus()).then_some(CODING_TOOLSET)
    }

    pub fn hidden_skill_categories(&self) -> &'static [&'static str] {
        if self.is_coding() {
            NON_CODING_SKILL_CATEGORIES
        } else {
            &[]
        }
    }

    pub fn system_blocks(&self) -> Vec<String> {
        if !self.is_coding() {
            return Vec::new();
        }
        let mut blocks = Vec::new();
        let mut guidance = CODING_AGENT_GUIDANCE.to_string();
        if let Some(line) = edit_format_line(self.model.as_deref()) {
            guidance.push('\n');
            guidance.push_str(line);
        }
        blocks.push(guidance);
        if let Some(root) = &self.workspace_root {
            if let Some(block) = build_coding_workspace_block(root) {
                blocks.push(block);
            }
        }
        blocks
    }
}

pub fn resolve_runtime_mode(
    platform: Option<&str>,
    cwd: Option<&Path>,
    raw_mode: Option<&str>,
    model: Option<&str>,
) -> RuntimeMode {
    let mode = CodingContextMode::parse(raw_mode);
    let resolved_cwd = resolve_cwd(cwd);
    let workspace_root = workspace_root(&resolved_cwd);
    let platform = normalize_platform(platform);
    let active = match mode {
        CodingContextMode::Off => false,
        CodingContextMode::On => true,
        CodingContextMode::Auto | CodingContextMode::Focus => {
            is_interactive_coding_platform(&platform) && workspace_root.is_some()
        }
    };
    RuntimeMode {
        profile: if active {
            ContextProfileKind::Coding
        } else {
            ContextProfileKind::General
        },
        mode,
        workspace_root: if active {
            workspace_root.or(Some(resolved_cwd))
        } else {
            None
        },
        model: model.map(str::to_string),
    }
}

pub fn coding_toolset_selection(
    platform: Option<&str>,
    cwd: Option<&Path>,
    raw_mode: Option<&str>,
) -> Option<&'static str> {
    resolve_runtime_mode(platform, cwd, raw_mode, None).toolset_selection()
}

pub fn coding_hidden_skill_categories(
    platform: Option<&str>,
    cwd: Option<&Path>,
    raw_mode: Option<&str>,
) -> &'static [&'static str] {
    resolve_runtime_mode(platform, cwd, raw_mode, None).hidden_skill_categories()
}

pub fn model_family(model: Option<&str>) -> Option<&'static str> {
    let lowered = model?.to_ascii_lowercase();
    if ["gpt", "codex"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return Some("patch");
    }
    if [
        "claude", "sonnet", "opus", "haiku", "gemini", "gemma", "deepseek", "qwen", "kimi", "glm",
        "grok", "hermes", "llama", "mistral", "devstral", "minimax",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        return Some("replace");
    }
    None
}

pub fn edit_format_line(model: Option<&str>) -> Option<&'static str> {
    match model_family(model) {
        Some("patch") => Some(EDIT_FORMAT_PATCH),
        Some("replace") => Some(EDIT_FORMAT_REPLACE),
        _ => None,
    }
}

pub fn build_coding_workspace_block(root: &Path) -> Option<String> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !root.exists() {
        return None;
    }
    let mut lines = vec![
        "## Workspace".to_string(),
        format!("Root: {}", root.display()),
    ];

    let manifests = detected_project_markers(&root);
    if !manifests.is_empty() {
        lines.push(format!("Project: {}", manifests.join(", ")));
    }

    let verify = verify_commands(&root);
    if !verify.is_empty() {
        lines.push(format!("Verify: {}", verify.join("; ")));
    }

    let context_files = CONTEXT_FILES
        .iter()
        .copied()
        .filter(|name| root.join(name).is_file())
        .collect::<Vec<_>>();
    if !context_files.is_empty() {
        lines.push(format!("Context files: {}", context_files.join(", ")));
    }

    if git_root(&root).as_deref() == Some(root.as_path()) {
        append_git_snapshot(&root, &mut lines);
    }

    (lines.len() > 2).then(|| lines.join("\n"))
}

pub fn detect_project_facts(root: &Path) -> ProjectFacts {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let manifests = project_manifest_names(&root);
    let package_managers = package_managers(&root);
    let verify_commands = verify_commands(&root);
    let context_files = CONTEXT_FILES
        .iter()
        .copied()
        .filter(|name| root.join(name).is_file())
        .map(str::to_string)
        .collect();

    ProjectFacts {
        root: root.display().to_string(),
        manifests,
        package_managers,
        verify_commands,
        context_files,
    }
}

pub fn project_facts_for(cwd: Option<&Path>) -> Option<ProjectFacts> {
    let resolved = resolve_cwd(cwd);
    let root = workspace_root(&resolved)?;
    Some(detect_project_facts(&root))
}

fn normalize_platform(platform: Option<&str>) -> String {
    platform
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
}

fn is_interactive_coding_platform(platform: &str) -> bool {
    INTERACTIVE_CODING_PLATFORMS.contains(&platform)
}

fn resolve_cwd(cwd: Option<&Path>) -> PathBuf {
    cwd.map(Path::to_path_buf)
        .or_else(|| std::env::var_os("TERMINAL_CWD").map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn workspace_root(cwd: &Path) -> Option<PathBuf> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let home = home_dir();
    let marker = marker_root(&cwd);
    if let Some(marker) = marker {
        if home.as_deref() != Some(marker.as_path()) {
            return Some(marker);
        }
    }
    let git = git_root(&cwd);
    if let Some(git) = git {
        if home.as_deref() != Some(git.as_path()) {
            return Some(git);
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir().and_then(|path| path.canonicalize().ok())
}

fn ancestors_nearest_first(start: &Path) -> Vec<PathBuf> {
    let mut out = vec![start.to_path_buf()];
    out.extend(start.ancestors().skip(1).map(Path::to_path_buf));
    out
}

fn marker_root(cwd: &Path) -> Option<PathBuf> {
    ancestors_nearest_first(cwd)
        .into_iter()
        .take(12)
        .find(|parent| {
            PROJECT_MARKERS
                .iter()
                .any(|marker| parent.join(marker).exists())
        })
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    ancestors_nearest_first(cwd)
        .into_iter()
        .find(|parent| parent.join(".git").exists())
}

fn detected_project_markers(root: &Path) -> Vec<String> {
    let mut markers = Vec::new();
    for marker in PROJECT_MARKERS {
        if root.join(marker).exists() {
            if *marker == "package.json" {
                if let Some(pm) = js_package_manager(root) {
                    markers.push(format!("package.json ({pm})"));
                } else {
                    markers.push((*marker).to_string());
                }
            } else if *marker == "pyproject.toml" {
                if let Some(pm) = python_package_manager(root) {
                    markers.push(format!("pyproject.toml ({pm})"));
                } else {
                    markers.push((*marker).to_string());
                }
            } else {
                markers.push((*marker).to_string());
            }
        }
    }
    markers
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
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    let Some(scripts) = value.get("scripts").and_then(|v| v.as_object()) else {
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

fn dedup_truncate(commands: Vec<String>) -> Vec<String> {
    let mut seen = BTreeMap::new();
    for command in commands {
        seen.entry(command).or_insert(());
    }
    seen.into_keys().take(MAX_VERIFY_COMMANDS).collect()
}

fn append_git_snapshot(root: &Path, lines: &mut Vec<String>) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--short", "--branch"])
        .output();
    let Ok(output) = status else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut status_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for line in stdout.lines() {
        if let Some(branch) = line.strip_prefix("## ") {
            if !branch.trim().is_empty() {
                lines.push(format!("Branch: {}", branch.trim()));
            }
            continue;
        }
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
        *status_counts.entry(key).or_default() += 1;
    }
    if status_counts.is_empty() {
        lines.push("Status: clean".to_string());
    } else {
        let rendered = status_counts
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Status: {rendered}"));
    }
    if let Some(commit) = git_last_commit(root) {
        lines.push(format!("Last commit: {commit}"));
    }
}

fn git_last_commit(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--oneline"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_init(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["init", "-q"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.email", "test@example.com"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.name", "Hermes Test"])
            .status()
            .unwrap();
        std::fs::write(path.join("README.md"), "test").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", "README.md"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["commit", "-qm", "init commit"])
            .status()
            .unwrap();
    }

    #[test]
    fn coding_detection_respects_platform_and_modes() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        assert!(
            resolve_runtime_mode(Some("cli"), Some(tmp.path()), Some("auto"), None).is_coding()
        );
        assert!(
            !resolve_runtime_mode(Some("telegram"), Some(tmp.path()), Some("auto"), None)
                .is_coding()
        );
        assert!(
            resolve_runtime_mode(Some("telegram"), Some(tmp.path()), Some("on"), None).is_coding()
        );
        assert!(
            !resolve_runtime_mode(Some("cli"), Some(tmp.path()), Some("off"), None).is_coding()
        );
    }

    #[test]
    fn focus_mode_selects_coding_toolset_only_inside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            coding_toolset_selection(Some("cli"), Some(tmp.path()), Some("focus")),
            None
        );
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        assert_eq!(
            coding_toolset_selection(Some("cli"), Some(tmp.path()), Some("focus")),
            Some(CODING_TOOLSET)
        );
        assert_eq!(
            coding_toolset_selection(Some("cli"), Some(tmp.path()), Some("auto")),
            None
        );
    }

    #[test]
    fn workspace_block_reports_verify_loop_facts() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","dev":"vite"}}"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "# rules").unwrap();
        let block = build_coding_workspace_block(tmp.path()).unwrap();
        assert!(block.contains("Workspace"));
        assert!(block.contains("package.json (pnpm)"));
        assert!(block.contains("pnpm run test"));
        assert!(block.contains("pnpm run lint"));
        assert!(!block.contains("run dev"));
        assert!(block.contains("Context files: AGENTS.md"));
        assert!(block.contains("Status:"));
    }

    #[test]
    fn project_facts_are_structured_from_workspace_detector() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","lint":"eslint .","dev":"vite"}}"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "# rules").unwrap();

        let facts = project_facts_for(Some(tmp.path())).expect("project facts");

        assert_eq!(
            facts.root,
            tmp.path().canonicalize().unwrap().display().to_string()
        );
        assert!(facts.manifests.contains(&"package.json".to_string()));
        assert_eq!(facts.package_managers, vec!["pnpm".to_string()]);
        assert!(facts.verify_commands.contains(&"pnpm run test".to_string()));
        assert!(facts.verify_commands.contains(&"pnpm run lint".to_string()));
        assert!(!facts.verify_commands.iter().any(|cmd| cmd.contains("dev")));
        assert_eq!(facts.context_files, vec!["AGENTS.md".to_string()]);

        let rendered = build_coding_workspace_block(tmp.path()).unwrap();
        for command in &facts.verify_commands {
            assert!(rendered.contains(command));
        }
    }

    #[test]
    fn marker_only_project_gets_snapshot_without_git() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        let mode = resolve_runtime_mode(Some("cli"), Some(tmp.path()), Some("auto"), None);
        assert!(mode.is_coding());
        let block = mode.system_blocks().join("\n");
        assert!(block.contains("Cargo.toml"));
        assert!(block.contains("cargo test"));
        assert!(!block.contains("Branch:"));
    }

    #[test]
    fn edit_format_family_detection_covers_open_and_closed_models() {
        assert_eq!(model_family(Some("openai/gpt-5.4")), Some("patch"));
        assert_eq!(model_family(Some("openai/codex-mini")), Some("patch"));
        for model in [
            "anthropic/claude-sonnet-4",
            "google/gemini-3-pro",
            "deepseek-v3.2",
            "qwen3-coder",
            "moonshot/kimi-k2",
            "nousresearch/hermes-4-405b",
        ] {
            assert_eq!(model_family(Some(model)), Some("replace"));
        }
        assert_eq!(model_family(Some("acme/foo-1")), None);
        assert_eq!(model_family(None), None);
    }
}
