//! Skill usage sidecar and curator provenance filters.
//!
//! Centralised via [`UsageStore`] — all `.usage.json` I/O resolves to a single
//! canonical skills directory so callers never need to guess the right path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::skill::SkillError;

pub const STATE_ACTIVE: &str = "active";
pub const STATE_STALE: &str = "stale";
pub const STATE_ARCHIVED: &str = "archived";

// ── Data types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillUsageRecord {
    #[serde(default)]
    pub use_count: u64,
    #[serde(default)]
    pub view_count: u64,
    #[serde(default)]
    pub patch_count: u64,
    #[serde(default = "default_state")]
    pub state: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub last_viewed_at: Option<String>,
    #[serde(default)]
    pub last_patched_at: Option<String>,
    #[serde(default)]
    pub agent_created: bool,
}

impl Default for SkillUsageRecord {
    fn default() -> Self {
        Self {
            use_count: 0,
            view_count: 0,
            patch_count: 0,
            state: STATE_ACTIVE.to_string(),
            pinned: false,
            archived_at: None,
            last_used_at: None,
            last_viewed_at: None,
            last_patched_at: None,
            agent_created: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillUsageReportRow {
    pub name: String,
    pub use_count: u64,
    pub view_count: u64,
    pub patch_count: u64,
    pub activity_count: u64,
    pub state: String,
    pub pinned: bool,
    pub archived_at: Option<String>,
    pub last_activity_at: Option<String>,
}

fn default_state() -> String {
    STATE_ACTIVE.to_string()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

// ── UsageStore ───────────────────────────────────────────────────────────

/// Centralised store for `.usage.json` operations.
///
/// All `.usage.json` I/O resolves to a single skills directory — callers no
/// longer need to pass a `skills_dir` parameter and can never accidentally
/// write to a legacy / wrong location.
///
/// # Construction
///
/// * `UsageStore::new()` — canonical directory (`hermes_config::skills_dir()`).
/// * `UsageStore::with_dir(path)` — explicit directory (useful for tests).
#[derive(Debug, Clone)]
pub struct UsageStore {
    skills_dir: PathBuf,
}

impl UsageStore {
    /// Create a store rooted at the canonical skills directory.
    pub fn new() -> Self {
        Self {
            skills_dir: hermes_config::skills_dir(),
        }
    }

    /// Create a store rooted at an explicit directory (e.g. a temp dir in
    /// tests).
    pub fn with_dir(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    /// The skills directory this store operates on.
    pub fn dir(&self) -> &Path {
        &self.skills_dir
    }
}

impl Default for UsageStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Internal helpers ────────────────────────────────────────────────────

struct UsageLock {
    path: PathBuf,
}

impl Drop for UsageLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl UsageStore {
    fn usage_file(&self) -> PathBuf {
        self.skills_dir.join(".usage.json")
    }

    fn usage_lock_file(&self) -> PathBuf {
        self.skills_dir.join(".usage.lock")
    }

    fn acquire_usage_lock(&self) -> Result<UsageLock, SkillError> {
        fs::create_dir_all(&self.skills_dir)?;
        let path = self.usage_lock_file();
        let start = Instant::now();
        let mut stale_cleaned = false;
        loop {
            match fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let _ = writeln!(file, "pid={}", std::process::id());
                    return Ok(UsageLock { path });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    // If we have been waiting long enough, try to clean a stale lock
                    // (the owning process likely crashed without running Drop).
                    if !stale_cleaned && start.elapsed() > Duration::from_secs(2) {
                        let _ = fs::remove_file(&path);
                        stale_cleaned = true;
                        continue;
                    }
                    if start.elapsed() > Duration::from_secs(20) {
                        let _ = fs::remove_file(&path);
                        return Err(SkillError::Io(format!(
                            "Timed out waiting for usage sidecar lock: {}",
                            path.display()
                        )));
                    }
                    thread::sleep(Duration::from_millis(15));
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    fn mutate_usage<F>(&self, skill_name: &str, mut f: F) -> Result<(), SkillError>
    where
        F: FnMut(&mut SkillUsageRecord),
    {
        let name = skill_name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let _lock = self.acquire_usage_lock()?;
        let mut usage = self.load_usage();
        let rec = usage.entry(name.to_string()).or_default();
        f(rec);
        self.save_usage(&usage)
    }
}

// ── Core I/O ─────────────────────────────────────────────────────────────

impl UsageStore {
    /// Load the entire `.usage.json` map, returning empty on any error.
    pub fn load_usage(&self) -> BTreeMap<String, SkillUsageRecord> {
        let path = self.usage_file();
        let Ok(raw) = fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        serde_json::from_str::<BTreeMap<String, SkillUsageRecord>>(&raw).unwrap_or_default()
    }

    /// Atomically persist the usage map.
    pub fn save_usage(&self, usage: &BTreeMap<String, SkillUsageRecord>) -> Result<(), SkillError> {
        fs::create_dir_all(&self.skills_dir)?;
        let path = self.usage_file();
        let tmp = self
            .skills_dir
            .join(format!(".usage_{}.tmp", std::process::id()));
        let body = serde_json::to_string_pretty(usage)
            .map_err(|e| SkillError::Parse(format!("Failed to encode usage sidecar: {e}")))?;
        fs::write(&tmp, body)?;
        // On Windows, rename can transiently fail if an antivirus scanner or file
        // indexer is holding the target file open.  Retry a few times.
        let mut last_err = None;
        for attempt in 0..5u32 {
            match fs::rename(&tmp, &path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 4 {
                        thread::sleep(Duration::from_millis(50 * (attempt as u64 + 1)));
                    }
                }
            }
        }
        // Final fallback: try a plain copy + remove (works even when rename fails).
        if let Ok(()) = fs::copy(&tmp, &path).map(|_| ()) {
            let _ = fs::remove_file(&tmp);
            return Ok(());
        }
        Err(last_err
            .map(|e| SkillError::Io(format!("Failed to rename usage sidecar: {e}")))
            .unwrap_or_else(|| SkillError::Io("Failed to save usage sidecar".into())))
    }
}

// ── Record helpers ───────────────────────────────────────────────────────

impl UsageStore {
    pub fn get_record(&self, skill_name: &str) -> SkillUsageRecord {
        self.load_usage()
            .get(skill_name)
            .cloned()
            .unwrap_or_default()
    }

    pub fn bump_view(&self, skill_name: &str) -> Result<(), SkillError> {
        self.mutate_usage(skill_name, |rec| {
            rec.view_count += 1;
            rec.last_viewed_at = Some(now_iso());
        })
    }

    pub fn bump_use(&self, skill_name: &str) -> Result<(), SkillError> {
        self.mutate_usage(skill_name, |rec| {
            rec.use_count += 1;
            rec.last_used_at = Some(now_iso());
        })
    }

    pub fn bump_patch(&self, skill_name: &str) -> Result<(), SkillError> {
        self.mutate_usage(skill_name, |rec| {
            rec.patch_count += 1;
            rec.last_patched_at = Some(now_iso());
        })
    }

    /// Mark a skill as agent-created.
    ///
    /// Refuses to mark protected (bundled / hub-installed) skills — name
    /// collisions between agent-created and built-in skills are always
    /// treated as errors because bundled skills must never appear in the
    /// curator candidate list.
    pub fn mark_agent_created(&self, skill_name: &str) -> Result<(), SkillError> {
        let name = skill_name.trim();
        if name.is_empty() {
            return Ok(());
        }
        if is_protected_skill(&self.skills_dir, name) {
            tracing::warn!(
                skill = %name,
                "refusing to mark protected skill as agent-created"
            );
            return Ok(());
        }
        let _lock = self.acquire_usage_lock()?;
        let mut usage = self.load_usage();
        let rec = usage.entry(name.to_string()).or_default();
        rec.agent_created = true;
        self.save_usage(&usage)
    }

    pub fn set_state(&self, skill_name: &str, state: &str) -> Result<(), SkillError> {
        if !matches!(state, STATE_ACTIVE | STATE_STALE | STATE_ARCHIVED) {
            return Ok(());
        }
        self.mutate_usage(skill_name, |rec| {
            rec.state = state.to_string();
            rec.archived_at = if state == STATE_ARCHIVED {
                Some(now_iso())
            } else {
                None
            };
        })
    }

    pub fn set_pinned(&self, skill_name: &str, pinned: bool) -> Result<(), SkillError> {
        self.mutate_usage(skill_name, |rec| {
            rec.pinned = pinned;
        })
    }

    pub fn forget(&self, skill_name: &str) -> Result<(), SkillError> {
        let name = skill_name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let _lock = self.acquire_usage_lock()?;
        let mut usage = self.load_usage();
        usage.remove(name);
        self.save_usage(&usage)
    }
}

// ── Agent-created queries ────────────────────────────────────────────────

impl UsageStore {
    /// Return agent-created skill names based solely on `.usage.json`.
    ///
    /// Before returning, this function **auto-heals** the usage data:
    /// - Entries where `agent_created=true` but the skill is actually
    ///   bundled/hub-installed → `agent_created` is reset to `false`.
    /// - Entries whose skill directory no longer exists on disk → the
    ///   entire usage record is removed.
    ///
    /// Modified usage is persisted immediately so later calls always
    /// see clean data.  The returned list contains **only** names where
    /// `agent_created` is `true` — no secondary filtering is applied.
    pub fn list_agent_created_skill_names(&self) -> Vec<String> {
        let mut usage = self.load_usage();
        let mut dirty = false;

        // Auto-heal: fix stale / incorrectly marked entries.
        let names_to_check: Vec<String> = usage
            .iter()
            .filter(|(_, rec)| rec.agent_created)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &names_to_check {
            if is_protected_skill(&self.skills_dir, name) {
                // Bundled / hub-installed skill incorrectly marked.
                if let Some(rec) = usage.get_mut(name) {
                    rec.agent_created = false;
                }
                dirty = true;
                tracing::warn!(
                    skill = %name,
                    "auto-fixed: agent_created=true reset for protected skill"
                );
            } else if find_skill_dir(&self.skills_dir, name).is_none() {
                // Skill directory no longer exists on disk — remove
                // the orphaned usage record.
                usage.remove(name);
                dirty = true;
                tracing::debug!(
                    skill = %name,
                    "auto-removed: usage record for non-existent skill directory"
                );
            }
        }

        if dirty {
            if let Err(e) = self.save_usage(&usage) {
                tracing::warn!("failed to persist auto-healed usage: {}", e);
            }
        }

        // Only .usage.json matters — no secondary filtering.
        let mut names: Vec<String> = usage
            .iter()
            .filter(|(_, rec)| rec.agent_created)
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    pub fn agent_created_report(&self) -> Vec<SkillUsageReportRow> {
        let usage = self.load_usage();
        let mut rows = Vec::new();
        for name in self.list_agent_created_skill_names() {
            let rec = usage.get(&name).cloned().unwrap_or_default();
            let last_activity_at = [
                rec.last_used_at.clone(),
                rec.last_viewed_at.clone(),
                rec.last_patched_at.clone(),
            ]
            .into_iter()
            .flatten()
            .max();
            rows.push(SkillUsageReportRow {
                name,
                use_count: rec.use_count,
                view_count: rec.view_count,
                patch_count: rec.patch_count,
                activity_count: rec.use_count + rec.view_count + rec.patch_count,
                state: rec.state,
                pinned: rec.pinned,
                archived_at: rec.archived_at,
                last_activity_at,
            });
        }
        rows
    }
}

// ── Skill lifecycle (archive / restore) ─────────────────────────────────

impl UsageStore {
    pub fn archive_skill(&self, skill_name: &str) -> Result<(bool, String), SkillError> {
        let name = skill_name.trim();
        if name.is_empty() {
            return Ok((false, "Skill name is required.".to_string()));
        }
        let Some(src) = find_skill_dir(&self.skills_dir, name) else {
            return Ok((false, format!("Skill '{name}' not found.")));
        };
        let archive_root = self.skills_dir.join(".archive");
        fs::create_dir_all(&archive_root)?;
        let mut dest = archive_root.join(name);
        if dest.exists() {
            dest = archive_root.join(format!("{}-{}", name, Utc::now().format("%Y%m%d%H%M%S")));
        }
        fs::rename(&src, &dest)?;
        self.set_state(name, STATE_ARCHIVED)?;
        Ok((true, format!("Skill '{name}' archived.")))
    }

    pub fn restore_skill(&self, skill_name: &str) -> Result<(bool, String), SkillError> {
        let name = skill_name.trim();
        if name.is_empty() {
            return Ok((false, "Skill name is required.".to_string()));
        }
        if is_protected_skill(&self.skills_dir, name)
            || find_skill_dir(&self.skills_dir, name).is_some()
        {
            return Ok((
                false,
                format!(
                    "Refusing to restore '{name}' because it would shadow an existing bundled, hub, or local skill."
                ),
            ));
        }
        let Some(src) = find_archived_skill_dir(&self.skills_dir, name) else {
            return Ok((false, format!("Archived skill '{name}' not found.")));
        };
        let dest = self.skills_dir.join(name);
        fs::rename(&src, &dest)?;
        self.set_state(name, STATE_ACTIVE)?;
        Ok((true, format!("Skill '{name}' restored.")))
    }
}

// ── File-system helpers (independent of UsageStore) ─────────────────────

pub(crate) fn read_skill_name_from_file(path: &Path, fallback: &str) -> String {
    let Ok(raw) = fs::read_to_string(path) else {
        return fallback.to_string();
    };
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return fallback.to_string();
    }
    let after_first = &trimmed[3..];
    let Some(end_idx) = after_first.find("\n---") else {
        return fallback.to_string();
    };
    let yaml = &after_first[..end_idx];
    let parsed = serde_yaml::from_str::<Value>(yaml).ok();
    parsed
        .as_ref()
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

pub(crate) fn read_skill_name_from_dir(skill_dir: &Path) -> String {
    let fallback = skill_dir
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("skill");
    read_skill_name_from_file(&skill_dir.join("SKILL.md"), fallback)
}

/// Compile-time canonical list of all bundled skill names (from SKILL.md frontmatter `name:`).
///
/// This is the **sole source of truth** for identifying built-in skills at
/// runtime. The `.bundled_manifest` file is never produced in production
/// (the `sync_skills()` code path that writes it is dead code), so the
/// protection mechanism must not depend on any runtime file for bundled
/// skill identification.
///
/// **Maintenance**: When a new internal skill is added to the `skills/`
/// directory, append its `name:` frontmatter value here. See AGENTS.md.
const BUNDLED_SKILL_NAMES_FALLBACK: &[&str] = &[
    "a-share-quant-tools",
    "airtable",
    "apple-notes",
    "apple-reminders",
    "architecture-diagram",
    "arxiv",
    "ascii-art",
    "ascii-video",
    "audiocraft-audio-generation",
    "baoyu-comic",
    "baoyu-infographic",
    "blogwatcher",
    "claude-code",
    "claude-design",
    "codebase-inspection",
    "codex",
    "comfyui",
    "debugging-hermes-tui-commands",
    "design-md",
    "dogfood",
    "equity-research",
    "dspy",
    "evaluating-llms-harness",
    "excalidraw",
    "findmy",
    "gif-search",
    "github-auth",
    "github-code-review",
    "github-issues",
    "github-pr-workflow",
    "github-repo-management",
    "global-market-watch",
    "godmode",
    "google-workspace",
    "heartmula",
    "hermes-agent",
    "hermes-agent-skill-authoring",
    "himalaya",
    "huggingface-hub",
    "humanizer",
    "ideation",
    "imessage",
    "jupyter-live-kernel",
    "kanban-orchestrator",
    "kanban-worker",
    "linear",
    "llama-cpp",
    "llm-wiki",
    "macos-computer-use",
    "manim-video",
    "market-news-sentinel",
    "maps",
    "minecraft-modpack-server",
    "multi-factor-backtest",
    "nano-pdf",
    "native-mcp",
    "node-inspect-debugger",
    "notion",
    "obliteratus",
    "obsidian",
    "ocr-and-documents",
    "opencode",
    "openhue",
    "p5js",
    "pixel-art",
    "plan",
    "pokemon-player",
    "polymarket",
    "popular-web-designs",
    "powerpoint",
    "pretext",
    "python-debugpy",
    "requesting-code-review",
    "research-paper-writing",
    "segment-anything-model",
    "serving-llms-vllm",
    "sketch",
    "songsee",
    "songwriting-and-ai-music",
    "spike",
    "spotify",
    "spot-quote",
    "subagent-driven-development",
    "systematic-debugging",
    "technical-indicators",
    "teams-meeting-pipeline",
    "test-driven-development",
    "touchdesigner-mcp",
    "trading-debate",
    "trading-cron",
    "trading-research",
    "webhook-subscriptions",
    "weights-and-biases",
    "writing-plans",
    "xurl",
    "youtube-content",
    "yuanbao",
];

fn bundled_skill_names(_skills_dir: &Path) -> BTreeSet<String> {
    // The compile-time constant is the sole source of truth.
    // `.bundled_manifest` is not read because `sync_skills()` is never
    // invoked from any production entry point — the manifest is never
    // created, making a runtime layer unreliable.
    BUNDLED_SKILL_NAMES_FALLBACK
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

#[derive(Debug, Deserialize)]
struct HubLock {
    #[serde(default)]
    installed: HubInstalledCollection,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HubInstalledCollection {
    Vec(Vec<HubInstalledEntry>),
    Map(BTreeMap<String, HubInstalledEntry>),
}

impl Default for HubInstalledCollection {
    fn default() -> Self {
        Self::Vec(Vec::new())
    }
}

#[derive(Debug, Default, Deserialize)]
struct HubInstalledEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    install_path: String,
    #[serde(flatten)]
    _rest: BTreeMap<String, Value>,
}

fn hub_installed_names(skills_dir: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Ok(raw) = fs::read_to_string(skills_dir.join(".hub").join("lock.json")) else {
        return names;
    };
    let Ok(lock) = serde_json::from_str::<HubLock>(&raw) else {
        return names;
    };
    let entries: Vec<(Option<String>, HubInstalledEntry)> = match lock.installed {
        HubInstalledCollection::Vec(entries) => {
            entries.into_iter().map(|entry| (None, entry)).collect()
        }
        HubInstalledCollection::Map(entries) => entries
            .into_iter()
            .map(|(name, entry)| (Some(name), entry))
            .collect(),
    };
    for (key, entry) in entries {
        if let Some(key) = key.filter(|v| !v.trim().is_empty()) {
            names.insert(key);
        }
        if !entry.name.trim().is_empty() {
            names.insert(entry.name.trim().to_string());
        }
        if !entry.install_path.trim().is_empty() {
            let path = Path::new(entry.install_path.trim());
            if let Some(base) = path.file_name().and_then(|v| v.to_str()) {
                names.insert(base.to_string());
            }
            let skill_dir = skills_dir.join(path);
            if skill_dir.join("SKILL.md").exists() {
                names.insert(read_skill_name_from_dir(&skill_dir));
            }
        }
    }
    names
}

pub fn is_protected_skill(skills_dir: &Path, skill_name: &str) -> bool {
    let name = skill_name.trim();
    if name.is_empty() {
        return false;
    }
    bundled_skill_names(skills_dir).contains(name) || hub_installed_names(skills_dir).contains(name)
}

pub fn is_agent_created(skills_dir: &Path, skill_name: &str) -> bool {
    !skill_name.trim().is_empty() && !is_protected_skill(skills_dir, skill_name)
}

fn find_skill_dir(skills_dir: &Path, skill_name: &str) -> Option<PathBuf> {
    let mut stack = vec![skills_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            if path.join("SKILL.md").exists() {
                let declared = read_skill_name_from_dir(&path);
                if name == skill_name || declared == skill_name {
                    return Some(path);
                }
            } else {
                stack.push(path);
            }
        }
    }
    None
}

fn find_archived_skill_dir(skills_dir: &Path, skill_name: &str) -> Option<PathBuf> {
    let archive_root = skills_dir.join(".archive");
    let mut stack = vec![archive_root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.join("SKILL.md").exists() {
                let dir_name = path.file_name().and_then(|v| v.to_str()).unwrap_or("");
                let declared = read_skill_name_from_dir(&path);
                if declared == skill_name
                    || dir_name == skill_name
                    || dir_name.starts_with(&format!("{skill_name}-"))
                {
                    return Some(path);
                }
            } else {
                stack.push(path);
            }
        }
    }
    None
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_store(dir: &tempfile::TempDir) -> UsageStore {
        UsageStore::with_dir(dir.path().to_path_buf())
    }

    fn write_skill(skills_dir: &Path, name: &str, category: Option<&str>) -> PathBuf {
        let dir = category
            .map(|cat| skills_dir.join(cat).join(name))
            .unwrap_or_else(|| skills_dir.join(name));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\n# body\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn save_load_and_missing_defaults() {
        let dir = tempdir().unwrap();
        let store = make_store(&dir);
        assert!(store.load_usage().is_empty());
        let mut data = BTreeMap::new();
        data.insert(
            "skill-a".to_string(),
            SkillUsageRecord {
                use_count: 3,
                ..Default::default()
            },
        );
        store.save_usage(&data).unwrap();
        assert_eq!(store.get_record("skill-a").use_count, 3);
        assert_eq!(store.get_record("missing").state, STATE_ACTIVE);
        assert!(!store.skills_dir.join(".usage_").exists());
    }

    #[test]
    fn bump_counters_and_forget() {
        let dir = tempdir().unwrap();
        let store = make_store(&dir);
        store.bump_view("x").unwrap();
        store.bump_use("x").unwrap();
        store.bump_patch("x").unwrap();
        let rec = store.get_record("x");
        assert_eq!(rec.view_count, 1);
        assert_eq!(rec.use_count, 1);
        assert_eq!(rec.patch_count, 1);
        assert!(rec.last_viewed_at.is_some());
        store.forget("x").unwrap();
        assert!(store.load_usage().is_empty());
    }

    #[test]
    fn protected_skills_can_get_usage_records() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        let store = make_store(&dir);
        fs::create_dir_all(skills.join(".hub")).unwrap();
        // "bundled" is not a real fallback name — use "yuanbao" instead.
        store.bump_view("yuanbao").unwrap();
        fs::write(
            skills.join(".hub").join("lock.json"),
            r#"{"installed":[{"name":"hubbed","install_path":"hubbed"}]}"#,
        )
        .unwrap();

        store.bump_view("bundled").unwrap();
        store.bump_use("hubbed").unwrap();
        store.set_state("yuanbao", STATE_ARCHIVED).unwrap();
        // Preset skills now receive normal usage records — only curator
        // operations are blocked via mark_agent_created / auto-heal.
        let usage = store.load_usage();
        assert!(!usage.is_empty());
        assert!(!is_agent_created(skills, "yuanbao"));
        assert!(!is_agent_created(skills, "hubbed"));
        assert!(is_agent_created(skills, "mine"));
    }

    #[test]
    fn hub_lock_install_path_frontmatter_name_is_protected() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        let skill_dir = skills.join("productivity").join("getnote");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: get-note-display\n---\n# body\n",
        )
        .unwrap();
        fs::create_dir_all(skills.join(".hub")).unwrap();
        fs::write(
            skills.join(".hub").join("lock.json"),
            r#"{"installed":{"getnote":{"source":"taps/main","install_path":"productivity/getnote"}}}"#,
        )
        .unwrap();
        assert!(is_protected_skill(skills, "getnote"));
        assert!(is_protected_skill(skills, "get-note-display"));
    }

    #[test]
    fn agent_created_report_requires_marker_and_excludes_protected() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        let store = make_store(&dir);
        write_skill(skills, "mine", None);
        write_skill(skills, "manual", None);
        store.mark_agent_created("mine").unwrap();
        store.bump_view("mine").unwrap();
        store.bump_view("manual").unwrap();

        let names = store.list_agent_created_skill_names();
        assert_eq!(names, vec!["mine"]);
        let report = store.agent_created_report();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].view_count, 1);
        assert_eq!(report[0].activity_count, 1);
    }

    #[test]
    fn archive_restore_and_collision_suffix() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        let store = make_store(&dir);
        write_skill(skills, "dup", None);
        let (ok, _) = store.archive_skill("dup").unwrap();
        assert!(ok);
        write_skill(skills, "dup", None);
        let (ok, _) = store.archive_skill("dup").unwrap();
        assert!(ok);
        let archived = fs::read_dir(skills.join(".archive"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(archived.iter().any(|name| name == "dup"));
        assert!(archived.iter().any(|name| name.starts_with("dup-")));

        let (ok, _) = store.restore_skill("dup").unwrap();
        assert!(ok);
        assert!(skills.join("dup").join("SKILL.md").exists());
    }

    #[test]
    fn archive_allows_protected_and_restore_refuses_shadowing() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        let store = make_store(&dir);
        write_skill(skills, "shared", None);
        store.archive_skill("shared").unwrap();
        write_skill(skills, "shared", None);
        // "shared" is not in the fallback list — use "yuanbao" to test
        // archive/restore interaction with a protected skill.
        // Create a skill dir so restore finds it on disk (shadowing guard).
        write_skill(skills, "yuanbao", None);
        // Restore refuses to shadow an existing skill (protected or not).
        let (ok, msg) = store.restore_skill("yuanbao").unwrap();
        assert!(
            !ok,
            "restore should refuse because yuanbao dir already exists"
        );
        assert!(msg.contains("shadow") || msg.contains("shadow") || msg.contains("Refusing"));
        // Archiving a protected skill is allowed — only curator operations
        // are blocked on protected skills.
        let (ok, _) = store.archive_skill("yuanbao").unwrap();
        assert!(ok);
    }

    #[test]
    fn mark_agent_created_rejects_protected() {
        let dir = tempdir().unwrap();
        let store = make_store(&dir);
        // "yuanbao" is in the compile-time constant → protected
        // (no manifest needed)
        store.mark_agent_created("yuanbao").unwrap();
        let rec = store.get_record("yuanbao");
        assert!(
            !rec.agent_created,
            "bundled skill should not be marked agent-created"
        );
    }

    #[test]
    fn list_agent_created_excludes_protected() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        let store = make_store(&dir);
        // "yuanbao" is in the compile-time constant → protected
        // (no manifest needed)
        // Create the skill dir so find_skill_dir doesn't filter it out
        write_skill(skills, "yuanbao", None);
        // Manually inject agent_created=true into .usage.json (bypass mark_agent_created guard)
        {
            let mut usage = store.load_usage();
            let rec = usage.entry("yuanbao".to_string()).or_default();
            rec.agent_created = true;
            store.save_usage(&usage).unwrap();
        }
        let names = store.list_agent_created_skill_names();
        assert!(
            !names.contains(&"yuanbao".to_string()),
            "protected skill with stale agent_created=true should be auto-fixed"
        );
        // Verify the auto-heal persisted: agent_created should now be false
        let usage = store.load_usage();
        let rec = usage.get("yuanbao").unwrap();
        assert!(
            !rec.agent_created,
            "auto-heal should have reset agent_created to false"
        );
    }

    #[test]
    fn list_agent_created_excludes_missing_dirs() {
        let dir = tempdir().unwrap();
        let store = make_store(&dir);
        // Mark as agent-created without creating the directory on disk
        store.mark_agent_created("ghost-skill").unwrap();
        let names = store.list_agent_created_skill_names();
        assert!(
            !names.contains(&"ghost-skill".to_string()),
            "orphaned usage record should be auto-removed by list_agent_created_skill_names"
        );
        // Verify the auto-heal persisted: the entry should be gone entirely
        let usage = store.load_usage();
        assert!(
            !usage.contains_key("ghost-skill"),
            "auto-heal should have removed the orphaned usage entry"
        );
    }

    #[test]
    fn compile_time_constant_protects_without_manifest() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        // No .bundled_manifest file — compile-time constant is the sole guard
        assert!(
            !skills.join(".bundled_manifest").exists(),
            "test precondition: no manifest"
        );
        // Fallback should recognise built-in skills as protected
        assert!(
            is_protected_skill(skills, "yuanbao"),
            "yuanbao should be protected via compile-time constant"
        );
        assert!(
            is_protected_skill(skills, "powerpoint"),
            "powerpoint should be protected via compile-time constant"
        );
        assert!(
            is_protected_skill(skills, "baoyu-comic"),
            "baoyu-comic should be protected via compile-time constant"
        );
        assert!(
            is_protected_skill(skills, "baoyu-infographic"),
            "baoyu-infographic should be protected via compile-time constant"
        );
        // Unknown skill should still not be protected
        assert!(
            !is_protected_skill(skills, "my-custom-skill"),
            "unknown skill should not be protected"
        );
        // Agent-created umbrella skills should also not be protected
        assert!(
            !is_protected_skill(skills, "baoyu"),
            "agent-created umbrella 'baoyu' is NOT a built-in name"
        );
        assert!(
            !is_protected_skill(skills, "software-engineering"),
            "agent-created umbrella 'software-engineering' is NOT a built-in name"
        );
    }
}
