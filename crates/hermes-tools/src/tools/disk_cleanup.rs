//! Rust-native disk cleanup for ephemeral Hermes files.
//!
//! This mirrors the bundled `plugins/disk-cleanup` behavior without loading
//! Python runtime code. Scope is limited to `HERMES_HOME` and `/tmp/hermes-*`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const STATE_DIR_NAME: &str = "disk-cleanup";
const TRACKED_FILE_NAME: &str = "tracked.json";
const LOG_FILE_NAME: &str = "cleanup.log";
const LARGE_FILE_BYTES: u64 = 500 * 1024 * 1024;

const TEST_PREFIXES: &[&str] = &["test_", "tmp_"];
const TEST_SUFFIXES: &[&str] = &[".test.py", ".test.js", ".test.ts", ".test.md"];
const PROTECTED_AUTO_TOP_LEVEL: &[&str] = &[
    "disk-cleanup",
    "logs",
    "memories",
    "sessions",
    "config.yaml",
    "skills",
    "plugins",
    ".env",
    "USER.md",
    "MEMORY.md",
    "SOUL.md",
    "auth.json",
    "hermes-agent",
];
const PROTECTED_EMPTY_TOP_LEVEL: &[&str] = &[
    "logs",
    "memories",
    "sessions",
    "cron",
    "cronjobs",
    "cache",
    "skills",
    "plugins",
    "disk-cleanup",
    "optional-skills",
    "hermes-agent",
    "backups",
    "profiles",
    ".worktrees",
];

pub const ALLOWED_CATEGORIES: &[&str] = &[
    "temp",
    "test",
    "research",
    "download",
    "chrome-profile",
    "cron-output",
    "other",
];

pub const HELP_TEXT: &str = "\
/disk-cleanup - ephemeral-file cleanup

Subcommands:
  status                     Per-category breakdown + top-10 largest
  dry-run                    Preview what quick/deep would delete
  quick                      Run safe cleanup now (no prompts)
  deep                       Run quick, then list items that need prompts
  track <path> <category>    Manually add a path to tracking
  forget <path>              Stop tracking a path (does not delete)

Categories: temp | test | research | download | chrome-profile | cron-output | other

All operations are scoped to HERMES_HOME and /tmp/hermes-*.
Test files are auto-tracked on write_file / terminal and auto-cleaned at session end.
";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackedItem {
    pub path: String,
    pub timestamp: String,
    pub category: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CleanupSummary {
    pub deleted: usize,
    pub empty_dirs: usize,
    pub freed: u64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct StatusSummary {
    pub categories: HashMap<String, CategorySummary>,
    pub top10: Vec<TopTrackedItem>,
    pub total_tracked: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CategorySummary {
    pub count: usize,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopTrackedItem {
    pub path: String,
    pub size: u64,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DeepSummary {
    pub quick: CleanupSummary,
    pub deep_deleted: usize,
    pub deep_freed: u64,
}

#[derive(Debug, Clone)]
pub struct DiskCleanup {
    hermes_home: PathBuf,
}

impl DiskCleanup {
    pub fn new(hermes_home: impl Into<PathBuf>) -> Self {
        Self {
            hermes_home: hermes_home.into(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(hermes_config::hermes_home())
    }

    pub fn hermes_home(&self) -> &Path {
        &self.hermes_home
    }

    pub fn state_dir(&self) -> PathBuf {
        self.hermes_home.join(STATE_DIR_NAME)
    }

    pub fn tracked_file(&self) -> PathBuf {
        self.state_dir().join(TRACKED_FILE_NAME)
    }

    pub fn log_file(&self) -> PathBuf {
        self.state_dir().join(LOG_FILE_NAME)
    }

    pub fn is_safe_path(&self, path: &Path) -> bool {
        let Ok(abs) = normalized_path(path) else {
            return false;
        };
        let home = fs::canonicalize(&self.hermes_home).unwrap_or_else(|_| {
            absolute_path(&self.hermes_home).unwrap_or(self.hermes_home.clone())
        });
        abs.starts_with(&home) || is_tmp_hermes_path(&abs)
    }

    pub fn load_tracked(&self) -> Vec<TrackedItem> {
        let tracked_file = self.tracked_file();
        if let Some(parent) = tracked_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if !tracked_file.exists() {
            return Vec::new();
        }

        match fs::read_to_string(&tracked_file)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<TrackedItem>>(&text).ok())
        {
            Some(items) => items,
            None => {
                let backup = backup_path(&tracked_file);
                if let Some(items) = fs::read_to_string(&backup)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Vec<TrackedItem>>(&text).ok())
                {
                    self.audit_log("WARN: tracked.json corrupted - restored from .bak");
                    return items;
                }
                self.audit_log("WARN: tracked.json corrupted, no backup - starting fresh");
                Vec::new()
            }
        }
    }

    pub fn save_tracked(&self, tracked: &[TrackedItem]) -> Result<(), ToolError> {
        let tracked_file = self.tracked_file();
        if let Some(parent) = tracked_file.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let tmp = tracked_file.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(tracked)
            .map_err(|e| ToolError::ExecutionFailed(format!("serialize tracked.json: {e}")))?;
        fs::write(&tmp, format!("{json}\n")).map_err(io_error)?;
        if tracked_file.exists() {
            fs::copy(&tracked_file, backup_path(&tracked_file)).map_err(io_error)?;
        }
        fs::rename(&tmp, &tracked_file).map_err(io_error)?;
        Ok(())
    }

    pub fn guess_category(&self, path: &Path) -> Option<&'static str> {
        if !self.is_safe_path(path) {
            return None;
        }

        if let Ok(abs) = normalized_path(path) {
            let home = fs::canonicalize(&self.hermes_home).unwrap_or_else(|_| {
                absolute_path(&self.hermes_home).unwrap_or(self.hermes_home.clone())
            });
            if let Ok(rel) = abs.strip_prefix(&home) {
                let mut parts = rel.components().filter_map(|c| c.as_os_str().to_str());
                let top = parts.next().unwrap_or("");
                if PROTECTED_AUTO_TOP_LEVEL.contains(&top) {
                    return None;
                }
                if top == "cron" || top == "cronjobs" {
                    return match parts.next() {
                        Some("output") => Some("cron-output"),
                        _ => None,
                    };
                }
                if top == "cache" {
                    return Some("temp");
                }
            }
        }

        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if TEST_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
            return Some("test");
        }
        if TEST_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
            return Some("test");
        }
        None
    }

    pub fn track(&self, path: &Path, category: &str) -> Result<bool, ToolError> {
        let category = if ALLOWED_CATEGORIES.contains(&category) {
            category
        } else {
            self.audit_log(&format!(
                "WARN: unknown category '{category}', using 'other'"
            ));
            "other"
        };

        if !path.exists() {
            self.audit_log(&format!("SKIP: {} (does not exist)", path.display()));
            return Ok(false);
        }
        let path = fs::canonicalize(path).map_err(io_error)?;
        if !self.is_safe_path(&path) {
            self.audit_log(&format!("REJECT: {} (outside HERMES_HOME)", path.display()));
            return Ok(false);
        }

        let size = fs::metadata(&path)
            .map(|m| if m.is_file() { m.len() } else { 0 })
            .unwrap_or(0);
        let mut tracked = self.load_tracked();
        let path_str = path.to_string_lossy().to_string();
        if tracked.iter().any(|item| item.path == path_str) {
            return Ok(false);
        }
        tracked.push(TrackedItem {
            path: path_str.clone(),
            timestamp: Utc::now().to_rfc3339(),
            category: category.to_string(),
            size,
        });
        self.save_tracked(&tracked)?;
        self.audit_log(&format!(
            "TRACKED: {} ({category}, {})",
            path.display(),
            fmt_size(size)
        ));
        Ok(true)
    }

    pub fn forget(&self, path: &Path) -> Result<usize, ToolError> {
        let target = comparable_path(path);
        let mut tracked = self.load_tracked();
        let before = tracked.len();
        tracked.retain(|item| comparable_path(Path::new(&item.path)) != target);
        let removed = before.saturating_sub(tracked.len());
        if removed > 0 {
            self.save_tracked(&tracked)?;
            self.audit_log(&format!("FORGOT: {} ({removed} entries)", target.display()));
        }
        Ok(removed)
    }

    pub fn dry_run(&self) -> (Vec<TrackedItem>, Vec<TrackedItem>) {
        let now = Utc::now();
        let mut auto = Vec::new();
        let mut prompt = Vec::new();
        for item in self.load_tracked() {
            if !Path::new(&item.path).exists() {
                continue;
            }
            let age_days = age_days(&item.timestamp, now).unwrap_or(0);
            match item.category.as_str() {
                "test" => auto.push(item),
                "temp" if age_days > 7 => auto.push(item),
                "cron-output" if age_days > 14 => auto.push(item),
                "research" if age_days > 30 => prompt.push(item),
                "chrome-profile" if age_days > 14 => prompt.push(item),
                _ if item.size > LARGE_FILE_BYTES => prompt.push(item),
                _ => {}
            }
        }
        (auto, prompt)
    }

    pub fn quick(&self) -> CleanupSummary {
        let tracked = self.load_tracked();
        let now = Utc::now();
        let mut deleted = 0usize;
        let mut freed = 0u64;
        let mut new_tracked = Vec::new();
        let mut errors = Vec::new();

        for item in tracked {
            let path = Path::new(&item.path);
            if !path.exists() {
                self.audit_log(&format!(
                    "STALE: {} (removed from tracking)",
                    path.display()
                ));
                continue;
            }
            let age_days = age_days(&item.timestamp, now).unwrap_or(0);
            let should_delete = item.category == "test"
                || (item.category == "temp" && age_days > 7)
                || (item.category == "cron-output" && age_days > 14);

            if should_delete {
                match remove_path(path) {
                    Ok(()) => {
                        freed = freed.saturating_add(item.size);
                        deleted += 1;
                        self.audit_log(&format!(
                            "DELETED: {} ({}, {})",
                            path.display(),
                            item.category,
                            fmt_size(item.size)
                        ));
                    }
                    Err(err) => {
                        self.audit_log(&format!("ERROR deleting {}: {err}", path.display()));
                        errors.push(format!("{}: {err}", path.display()));
                        new_tracked.push(item);
                    }
                }
            } else {
                new_tracked.push(item);
            }
        }

        let empty_dirs = self.remove_empty_dirs();
        if let Err(err) = self.save_tracked(&new_tracked) {
            errors.push(err.to_string());
        }
        self.audit_log(&format!(
            "QUICK_SUMMARY: {deleted} files, {empty_dirs} dirs, {}",
            fmt_size(freed)
        ));

        CleanupSummary {
            deleted,
            empty_dirs,
            freed,
            errors,
        }
    }

    pub fn deep_without_confirm(&self) -> DeepSummary {
        DeepSummary {
            quick: self.quick(),
            deep_deleted: 0,
            deep_freed: 0,
        }
    }

    pub fn status(&self) -> StatusSummary {
        let tracked = self.load_tracked();
        let mut categories: HashMap<String, CategorySummary> = HashMap::new();
        let mut existing = Vec::new();

        for item in &tracked {
            let entry = categories.entry(item.category.clone()).or_default();
            entry.count += 1;
            entry.size = entry.size.saturating_add(item.size);
            if Path::new(&item.path).exists() {
                existing.push(TopTrackedItem {
                    path: item.path.clone(),
                    size: item.size,
                    category: item.category.clone(),
                });
            }
        }
        existing.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
        existing.truncate(10);

        StatusSummary {
            categories,
            top10: existing,
            total_tracked: tracked.len(),
        }
    }

    fn remove_empty_dirs(&self) -> usize {
        let mut dirs = Vec::new();
        collect_dirs_postorder(&self.hermes_home, &mut dirs);
        let mut removed = 0usize;
        for dir in dirs {
            if dir == self.hermes_home {
                continue;
            }
            let Ok(rel) = dir.strip_prefix(&self.hermes_home) else {
                continue;
            };
            let parts: Vec<_> = rel
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect();
            if parts.len() == 1 && PROTECTED_EMPTY_TOP_LEVEL.contains(&parts[0]) {
                continue;
            }
            match fs::read_dir(&dir) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        continue;
                    }
                    if fs::remove_dir(&dir).is_ok() {
                        removed += 1;
                        self.audit_log(&format!("DELETED: {} (empty dir)", dir.display()));
                    }
                }
                _ => {}
            }
        }
        removed
    }

    fn audit_log(&self, message: &str) {
        let log_file = self.log_file();
        if let Some(parent) = log_file.parent() {
            if fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S");
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
        {
            let _ = writeln!(file, "[{timestamp}] {message}");
        }
    }
}

pub fn fmt_size(mut n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    for unit in units {
        if n < 1024 {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
        n /= 1024;
    }
    format!("{value:.1} PB")
}

pub fn format_status(status: &StatusSummary) -> String {
    let mut lines = vec![
        format!("{:<20} {:>6}  {:>10}", "Category", "Files", "Size"),
        "-".repeat(40),
    ];
    let mut categories: Vec<_> = status.categories.iter().collect();
    categories.sort_by(|(left_name, left), (right_name, right)| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left_name.cmp(right_name))
    });
    for (category, summary) in categories {
        lines.push(format!(
            "{category:<20} {:>6}  {:>10}",
            summary.count,
            fmt_size(summary.size)
        ));
    }
    if status.categories.is_empty() {
        lines.push("(nothing tracked yet)".to_string());
    }
    lines.push(String::new());
    lines.push("Top 10 largest tracked files:".to_string());
    if status.top10.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        for (idx, item) in status.top10.iter().enumerate() {
            lines.push(format!(
                "  {:>2}. {:>8}  [{}]  {}",
                idx + 1,
                fmt_size(item.size),
                item.category,
                item.path
            ));
        }
    }
    lines.join("\n")
}

pub fn handle_slash_args(cleaner: &DiskCleanup, args: &[&str]) -> String {
    let subcommand = args.first().copied().unwrap_or("help");
    if matches!(subcommand, "help" | "-h" | "--help") {
        return HELP_TEXT.to_string();
    }

    match subcommand {
        "status" => format_status(&cleaner.status()),
        "dry-run" => {
            let (auto, prompt) = cleaner.dry_run();
            let auto_size: u64 = auto.iter().map(|item| item.size).sum();
            let prompt_size: u64 = prompt.iter().map(|item| item.size).sum();
            let mut lines = vec![
                "Dry-run preview (nothing deleted):".to_string(),
                format!(
                    "  Auto-delete : {} files ({})",
                    auto.len(),
                    fmt_size(auto_size)
                ),
            ];
            for item in &auto {
                lines.push(format!("    [{}] {}", item.category, item.path));
            }
            lines.push(format!(
                "  Needs prompt: {} files ({})",
                prompt.len(),
                fmt_size(prompt_size)
            ));
            for item in &prompt {
                lines.push(format!("    [{}] {}", item.category, item.path));
            }
            lines.push(format!(
                "\n  Total potential: {}",
                fmt_size(auto_size.saturating_add(prompt_size))
            ));
            lines.join("\n")
        }
        "quick" => format_cleanup_summary(&cleaner.quick()),
        "deep" => {
            let quick = cleaner.quick();
            let (_auto, prompt) = cleaner.dry_run();
            let mut lines = vec![format_cleanup_summary(&quick)];
            if !prompt.is_empty() {
                let size: u64 = prompt.iter().map(|item| item.size).sum();
                lines.push(format!(
                    "\n{} item(s) need confirmation ({}):",
                    prompt.len(),
                    fmt_size(size)
                ));
                for item in prompt {
                    lines.push(format!("  [{}] {}", item.category, item.path));
                }
                lines.push(
                    "\nRun `/disk-cleanup forget <path>` to skip, or delete manually via terminal."
                        .to_string(),
                );
            }
            lines.join("\n")
        }
        "track" => {
            if args.len() < 3 {
                return "Usage: /disk-cleanup track <path> <category>".to_string();
            }
            let category = args[2];
            if !ALLOWED_CATEGORIES.contains(&category) {
                return format!(
                    "Unknown category '{category}'. Allowed: {:?}",
                    ALLOWED_CATEGORIES
                );
            }
            match cleaner.track(Path::new(args[1]), category) {
                Ok(true) => format!("Tracked {} as '{category}'.", args[1]),
                Ok(false) => format!(
                    "Not tracked (already present, missing, or outside HERMES_HOME): {}",
                    args[1]
                ),
                Err(err) => format!("Failed to track {}: {err}", args[1]),
            }
        }
        "forget" => {
            if args.len() < 2 {
                return "Usage: /disk-cleanup forget <path>".to_string();
            }
            match cleaner.forget(Path::new(args[1])) {
                Ok(0) => format!("Not found in tracking: {}", args[1]),
                Ok(n) => format!(
                    "Removed {n} tracking entr{} for {}.",
                    if n == 1 { "y" } else { "ies" },
                    args[1]
                ),
                Err(err) => format!("Failed to forget {}: {err}", args[1]),
            }
        }
        _ => format!("Unknown subcommand: {subcommand}\n\n{HELP_TEXT}"),
    }
}

fn format_cleanup_summary(summary: &CleanupSummary) -> String {
    let mut base = format!(
        "[disk-cleanup] Cleaned {} files + {} empty dirs, freed {}.",
        summary.deleted,
        summary.empty_dirs,
        fmt_size(summary.freed)
    );
    if !summary.errors.is_empty() {
        base.push_str(&format!(
            "\n  {} error(s); see cleanup.log.",
            summary.errors.len()
        ));
    }
    base
}

#[derive(Debug, Clone)]
pub struct DiskCleanupAutoTracker {
    cleaner: DiskCleanup,
    recent_test_tracks: Arc<Mutex<HashSet<String>>>,
}

impl DiskCleanupAutoTracker {
    pub fn new(cleaner: DiskCleanup) -> Self {
        Self {
            cleaner,
            recent_test_tracks: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn post_tool_call_success(&self, tool_name: &str, params: &Value, output: &str) {
        for candidate in extract_candidate_paths(tool_name, params, output) {
            let path = expand_user(candidate);
            let Some(category) = self.cleaner.guess_category(&path) else {
                continue;
            };
            let Ok(true) = self.cleaner.track(&path, category) else {
                continue;
            };
            if category == "test" {
                if let Ok(mut tracked) = self.recent_test_tracks.lock() {
                    tracked.insert(comparable_path(&path).to_string_lossy().to_string());
                }
            }
        }
    }

    pub fn on_session_end(&self) -> Option<CleanupSummary> {
        let should_run = self
            .recent_test_tracks
            .lock()
            .map(|mut tracked| {
                let should_run = !tracked.is_empty();
                tracked.clear();
                should_run
            })
            .unwrap_or(false);
        if should_run {
            Some(self.cleaner.quick())
        } else {
            None
        }
    }
}

impl Default for DiskCleanupAutoTracker {
    fn default() -> Self {
        Self::new(DiskCleanup::from_env())
    }
}

pub struct DiskCleanupHandler {
    cleaner: DiskCleanup,
}

impl DiskCleanupHandler {
    pub fn new(cleaner: DiskCleanup) -> Self {
        Self { cleaner }
    }
}

impl Default for DiskCleanupHandler {
    fn default() -> Self {
        Self::new(DiskCleanup::from_env())
    }
}

#[async_trait]
impl ToolHandler for DiskCleanupHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "status" => Ok(json!(self.cleaner.status()).to_string()),
            "dry_run" | "dry-run" => {
                let (auto, prompt) = self.cleaner.dry_run();
                Ok(json!({ "auto_delete": auto, "needs_prompt": prompt }).to_string())
            }
            "quick" => Ok(json!(self.cleaner.quick()).to_string()),
            "deep" => Ok(json!(self.cleaner.deep_without_confirm()).to_string()),
            "track" => {
                let path = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;
                let category =
                    params
                        .get("category")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            ToolError::InvalidParams("Missing 'category' parameter".into())
                        })?;
                if !ALLOWED_CATEGORIES.contains(&category) {
                    return Err(ToolError::InvalidParams(format!(
                        "Unknown category '{category}'. Allowed: {ALLOWED_CATEGORIES:?}"
                    )));
                }
                let tracked = self.cleaner.track(Path::new(path), category)?;
                Ok(json!({ "tracked": tracked, "path": path, "category": category }).to_string())
            }
            "forget" => {
                let path = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'path' parameter".into()))?;
                let removed = self.cleaner.forget(Path::new(path))?;
                Ok(json!({ "removed": removed, "path": path }).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{other}'. Use status, dry_run, quick, deep, track, or forget."
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Disk cleanup action: status, dry_run, quick, deep, track, or forget",
                "enum": ["status", "dry_run", "quick", "deep", "track", "forget"]
            }),
        );
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Path for track/forget actions"
            }),
        );
        props.insert(
            "category".into(),
            json!({
                "type": "string",
                "description": "Tracking category for track action",
                "enum": ALLOWED_CATEGORIES
            }),
        );

        tool_schema(
            "disk_cleanup",
            "Track, inspect, and clean ephemeral Hermes files under HERMES_HOME or /tmp/hermes-*.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

fn extract_candidate_paths(tool_name: &str, params: &Value, output: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    match tool_name {
        "write_file" | "patch" => {
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                candidates.push(path.to_string());
            }
        }
        "terminal" => {
            if let Some(command) = params.get("command").and_then(|v| v.as_str()) {
                candidates.extend(extract_absolute_like_paths(command));
            }
            if output.len() < 4096 {
                candidates.extend(extract_absolute_like_paths(output));
            }
        }
        _ => {}
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn extract_absolute_like_paths(text: &str) -> Vec<String> {
    static PATH_RE: OnceLock<Regex> = OnceLock::new();
    let regex =
        PATH_RE.get_or_init(|| Regex::new(r#"(?m)(?:^|\s)(/[^\s'"`]+|~/[^\s'"`]+)"#).unwrap());
    regex
        .captures_iter(text)
        .filter_map(|caps| caps.get(1).map(|m| trim_path_token(m.as_str()).to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn trim_path_token(token: &str) -> &str {
    token.trim_end_matches(|c: char| matches!(c, ',' | ';' | ':' | ')' | ']' | '}'))
}

fn expand_user(path: String) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn io_error(err: io::Error) -> ToolError {
    ToolError::ExecutionFailed(err.to_string())
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn normalized_path(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).or_else(|_| absolute_path(path))
}

fn comparable_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| absolute_path(path).unwrap_or_else(|_| path.into()))
}

fn is_tmp_hermes_path(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    match parts.as_slice() {
        ["tmp", rest @ ..] | ["private", "tmp", rest @ ..] => {
            rest.first().is_some_and(|part| part.starts_with("hermes-"))
        }
        _ => false,
    }
}

fn age_days(timestamp: &str, now: DateTime<Utc>) -> Option<i64> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| now.signed_duration_since(dt.with_timezone(&Utc)).num_days())
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn collect_dirs_postorder(root: &Path, dirs: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dirs_postorder(&path, dirs);
            dirs.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cleaner(tmp: &TempDir) -> DiskCleanup {
        let home = tmp.path().join(".hermes-agent-ultra");
        fs::create_dir_all(&home).unwrap();
        DiskCleanup::new(home)
    }

    #[test]
    fn guess_category_tracks_only_cron_output_subtree() {
        let tmp = TempDir::new().unwrap();
        let cleaner = cleaner(&tmp);
        let cron_dir = cleaner.hermes_home().join("cron");
        fs::create_dir_all(&cron_dir).unwrap();
        let jobs = cron_dir.join("jobs.json");
        fs::write(&jobs, "[]").unwrap();
        let lock = cron_dir.join(".tick.lock");
        fs::write(&lock, "").unwrap();
        let output_dir = cron_dir.join("output/job-1");
        fs::create_dir_all(&output_dir).unwrap();
        let output = output_dir.join("run.md");
        fs::write(&output, "ok").unwrap();

        assert_eq!(cleaner.guess_category(&jobs), None);
        assert_eq!(cleaner.guess_category(&lock), None);
        assert_eq!(cleaner.guess_category(&output), Some("cron-output"));
    }

    #[test]
    fn track_then_quick_deletes_test_file() {
        let tmp = TempDir::new().unwrap();
        let cleaner = cleaner(&tmp);
        let path = cleaner.hermes_home().join("test_cleanup.rs");
        fs::write(&path, "x").unwrap();

        assert!(cleaner.track(&path, "test").unwrap());
        let summary = cleaner.quick();

        assert_eq!(summary.deleted, 1);
        assert!(!path.exists());
    }

    #[test]
    fn quick_preserves_protected_top_level_dirs() {
        let tmp = TempDir::new().unwrap();
        let cleaner = cleaner(&tmp);
        for dir in ["logs", "memories", "sessions", "cron", "cache"] {
            fs::create_dir_all(cleaner.hermes_home().join(dir)).unwrap();
        }

        cleaner.quick();

        for dir in ["logs", "memories", "sessions", "cron", "cache"] {
            assert!(
                cleaner.hermes_home().join(dir).exists(),
                "{dir} should remain"
            );
        }
    }

    #[test]
    fn auto_tracker_tracks_write_file_and_session_end_deletes_test() {
        let tmp = TempDir::new().unwrap();
        let cleaner = cleaner(&tmp);
        let tracker = DiskCleanupAutoTracker::new(cleaner.clone());
        let path = cleaner.hermes_home().join("test_created.rs");
        fs::write(&path, "x").unwrap();

        tracker.post_tool_call_success(
            "write_file",
            &json!({"path": path.to_string_lossy()}),
            "ok",
        );
        let tracked = cleaner.load_tracked();
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].category, "test");

        let summary = tracker.on_session_end().unwrap();
        assert_eq!(summary.deleted, 1);
        assert!(!path.exists());
    }

    #[test]
    fn auto_tracker_extracts_terminal_output_paths() {
        let tmp = TempDir::new().unwrap();
        let cleaner = cleaner(&tmp);
        let tracker = DiskCleanupAutoTracker::new(cleaner.clone());
        let path = cleaner.hermes_home().join("tmp_created.log");
        fs::write(&path, "x").unwrap();

        tracker.post_tool_call_success(
            "terminal",
            &json!({"command": format!("touch {}", path.display())}),
            &format!("created {}\n", path.display()),
        );

        let tracked = cleaner.load_tracked();
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].category, "test");
    }

    #[tokio::test]
    async fn handler_tracks_and_forgets() {
        let tmp = TempDir::new().unwrap();
        let cleaner = cleaner(&tmp);
        let handler = DiskCleanupHandler::new(cleaner.clone());
        let path = cleaner.hermes_home().join("a.tmp");
        fs::write(&path, "x").unwrap();

        let out = handler
            .execute(json!({
                "action": "track",
                "path": path.to_string_lossy(),
                "category": "temp",
            }))
            .await
            .unwrap();
        assert!(out.contains(r#""tracked":true"#));

        let out = handler
            .execute(json!({
                "action": "forget",
                "path": path.to_string_lossy(),
            }))
            .await
            .unwrap();
        assert!(out.contains(r#""removed":1"#));
    }
}
