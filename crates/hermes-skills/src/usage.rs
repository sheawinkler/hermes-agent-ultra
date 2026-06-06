//! Skill usage sidecar and curator provenance filters.

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

pub fn usage_file(skills_dir: &Path) -> PathBuf {
    skills_dir.join(".usage.json")
}

fn usage_lock_file(skills_dir: &Path) -> PathBuf {
    skills_dir.join(".usage.lock")
}

struct UsageLock {
    path: PathBuf,
}

impl Drop for UsageLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn acquire_usage_lock(skills_dir: &Path) -> Result<UsageLock, SkillError> {
    fs::create_dir_all(skills_dir)?;
    let path = usage_lock_file(skills_dir);
    let start = Instant::now();
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
                if start.elapsed() > Duration::from_secs(20) {
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

pub fn load_usage(skills_dir: &Path) -> BTreeMap<String, SkillUsageRecord> {
    let path = usage_file(skills_dir);
    let Ok(raw) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, SkillUsageRecord>>(&raw).unwrap_or_default()
}

pub fn save_usage(
    skills_dir: &Path,
    usage: &BTreeMap<String, SkillUsageRecord>,
) -> Result<(), SkillError> {
    fs::create_dir_all(skills_dir)?;
    let path = usage_file(skills_dir);
    let tmp = skills_dir.join(format!(".usage_{}.tmp", std::process::id()));
    let body = serde_json::to_string_pretty(usage)
        .map_err(|e| SkillError::Parse(format!("Failed to encode usage sidecar: {e}")))?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn mutate_usage<F>(skills_dir: &Path, skill_name: &str, mut f: F) -> Result<(), SkillError>
where
    F: FnMut(&mut SkillUsageRecord),
{
    let name = skill_name.trim();
    if name.is_empty() || is_protected_skill(skills_dir, name) {
        return Ok(());
    }
    let _lock = acquire_usage_lock(skills_dir)?;
    let mut usage = load_usage(skills_dir);
    let rec = usage.entry(name.to_string()).or_default();
    f(rec);
    save_usage(skills_dir, &usage)
}

pub fn get_record(skills_dir: &Path, skill_name: &str) -> SkillUsageRecord {
    load_usage(skills_dir)
        .get(skill_name)
        .cloned()
        .unwrap_or_default()
}

pub fn bump_view(skills_dir: &Path, skill_name: &str) -> Result<(), SkillError> {
    mutate_usage(skills_dir, skill_name, |rec| {
        rec.view_count += 1;
        rec.last_viewed_at = Some(now_iso());
    })
}

pub fn bump_use(skills_dir: &Path, skill_name: &str) -> Result<(), SkillError> {
    mutate_usage(skills_dir, skill_name, |rec| {
        rec.use_count += 1;
        rec.last_used_at = Some(now_iso());
    })
}

pub fn bump_patch(skills_dir: &Path, skill_name: &str) -> Result<(), SkillError> {
    mutate_usage(skills_dir, skill_name, |rec| {
        rec.patch_count += 1;
        rec.last_patched_at = Some(now_iso());
    })
}

pub fn mark_agent_created(skills_dir: &Path, skill_name: &str) -> Result<(), SkillError> {
    mutate_usage(skills_dir, skill_name, |rec| {
        rec.agent_created = true;
    })
}

pub fn set_state(skills_dir: &Path, skill_name: &str, state: &str) -> Result<(), SkillError> {
    if !matches!(state, STATE_ACTIVE | STATE_STALE | STATE_ARCHIVED) {
        return Ok(());
    }
    mutate_usage(skills_dir, skill_name, |rec| {
        rec.state = state.to_string();
        rec.archived_at = if state == STATE_ARCHIVED {
            Some(now_iso())
        } else {
            None
        };
    })
}

pub fn set_pinned(skills_dir: &Path, skill_name: &str, pinned: bool) -> Result<(), SkillError> {
    mutate_usage(skills_dir, skill_name, |rec| {
        rec.pinned = pinned;
    })
}

pub fn forget(skills_dir: &Path, skill_name: &str) -> Result<(), SkillError> {
    let name = skill_name.trim();
    if name.is_empty() {
        return Ok(());
    }
    let _lock = acquire_usage_lock(skills_dir)?;
    let mut usage = load_usage(skills_dir);
    usage.remove(name);
    save_usage(skills_dir, &usage)
}

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

fn bundled_skill_names(skills_dir: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Ok(raw) = fs::read_to_string(skills_dir.join(".bundled_manifest")) else {
        return names;
    };
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let name = line.split_once(':').map(|(name, _)| name).unwrap_or(line);
        if !name.trim().is_empty() {
            names.insert(name.trim().to_string());
        }
    }
    names
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

pub fn list_agent_created_skill_names(skills_dir: &Path) -> Vec<String> {
    let usage = load_usage(skills_dir);
    let mut names: Vec<String> = usage
        .iter()
        .filter_map(|(name, rec)| {
            if rec.agent_created && is_agent_created(skills_dir, name) {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

pub fn agent_created_report(skills_dir: &Path) -> Vec<SkillUsageReportRow> {
    let usage = load_usage(skills_dir);
    let mut rows = Vec::new();
    for name in list_agent_created_skill_names(skills_dir) {
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

pub fn archive_skill(skills_dir: &Path, skill_name: &str) -> Result<(bool, String), SkillError> {
    let name = skill_name.trim();
    if name.is_empty() {
        return Ok((false, "Skill name is required.".to_string()));
    }
    if is_protected_skill(skills_dir, name) {
        return Ok((
            false,
            format!("Skill '{name}' is bundled or hub-installed and cannot be archived."),
        ));
    }
    let Some(src) = find_skill_dir(skills_dir, name) else {
        return Ok((false, format!("Skill '{name}' not found.")));
    };
    let archive_root = skills_dir.join(".archive");
    fs::create_dir_all(&archive_root)?;
    let mut dest = archive_root.join(name);
    if dest.exists() {
        dest = archive_root.join(format!("{}-{}", name, Utc::now().format("%Y%m%d%H%M%S")));
    }
    fs::rename(&src, &dest)?;
    set_state(skills_dir, name, STATE_ARCHIVED)?;
    Ok((true, format!("Skill '{name}' archived.")))
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

pub fn restore_skill(skills_dir: &Path, skill_name: &str) -> Result<(bool, String), SkillError> {
    let name = skill_name.trim();
    if name.is_empty() {
        return Ok((false, "Skill name is required.".to_string()));
    }
    if is_protected_skill(skills_dir, name) || find_skill_dir(skills_dir, name).is_some() {
        return Ok((
            false,
            format!("Refusing to restore '{name}' because it would shadow an existing bundled, hub, or local skill."),
        ));
    }
    let Some(src) = find_archived_skill_dir(skills_dir, name) else {
        return Ok((false, format!("Archived skill '{name}' not found.")));
    };
    let dest = skills_dir.join(name);
    fs::rename(&src, &dest)?;
    set_state(skills_dir, name, STATE_ACTIVE)?;
    Ok((true, format!("Skill '{name}' restored.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        let skills = dir.path();
        assert!(load_usage(skills).is_empty());
        let mut data = BTreeMap::new();
        data.insert(
            "skill-a".to_string(),
            SkillUsageRecord {
                use_count: 3,
                ..Default::default()
            },
        );
        save_usage(skills, &data).unwrap();
        assert_eq!(get_record(skills, "skill-a").use_count, 3);
        assert_eq!(get_record(skills, "missing").state, STATE_ACTIVE);
        assert!(!usage_file(skills)
            .parent()
            .unwrap()
            .join(".usage_")
            .exists());
    }

    #[test]
    fn bump_counters_and_forget() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        bump_view(skills, "x").unwrap();
        bump_use(skills, "x").unwrap();
        bump_patch(skills, "x").unwrap();
        let rec = get_record(skills, "x");
        assert_eq!(rec.view_count, 1);
        assert_eq!(rec.use_count, 1);
        assert_eq!(rec.patch_count, 1);
        assert!(rec.last_viewed_at.is_some());
        forget(skills, "x").unwrap();
        assert!(load_usage(skills).is_empty());
    }

    #[test]
    fn protected_skills_do_not_get_usage_records() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        fs::create_dir_all(skills.join(".hub")).unwrap();
        fs::write(skills.join(".bundled_manifest"), "bundled:abc\n").unwrap();
        fs::write(
            skills.join(".hub").join("lock.json"),
            r#"{"installed":[{"name":"hubbed","install_path":"hubbed"}]}"#,
        )
        .unwrap();

        bump_view(skills, "bundled").unwrap();
        bump_use(skills, "hubbed").unwrap();
        set_state(skills, "bundled", STATE_ARCHIVED).unwrap();
        assert!(load_usage(skills).is_empty());
        assert!(!is_agent_created(skills, "bundled"));
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
        write_skill(skills, "mine", None);
        write_skill(skills, "manual", None);
        mark_agent_created(skills, "mine").unwrap();
        bump_view(skills, "mine").unwrap();
        bump_view(skills, "manual").unwrap();

        let names = list_agent_created_skill_names(skills);
        assert_eq!(names, vec!["mine"]);
        let report = agent_created_report(skills);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].view_count, 1);
        assert_eq!(report[0].activity_count, 1);
    }

    #[test]
    fn archive_restore_and_collision_suffix() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        write_skill(skills, "dup", None);
        let (ok, _) = archive_skill(skills, "dup").unwrap();
        assert!(ok);
        write_skill(skills, "dup", None);
        let (ok, _) = archive_skill(skills, "dup").unwrap();
        assert!(ok);
        let archived = fs::read_dir(skills.join(".archive"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(archived.iter().any(|name| name == "dup"));
        assert!(archived.iter().any(|name| name.starts_with("dup-")));

        let (ok, _) = restore_skill(skills, "dup").unwrap();
        assert!(ok);
        assert!(skills.join("dup").join("SKILL.md").exists());
    }

    #[test]
    fn archive_refuses_protected_and_restore_refuses_shadowing() {
        let dir = tempdir().unwrap();
        let skills = dir.path();
        write_skill(skills, "shared", None);
        archive_skill(skills, "shared").unwrap();
        write_skill(skills, "shared", None);
        fs::write(skills.join(".bundled_manifest"), "shared:abc\n").unwrap();
        let (ok, msg) = restore_skill(skills, "shared").unwrap();
        assert!(!ok);
        assert!(msg.contains("shadow"));
        let (ok, _) = archive_skill(skills, "shared").unwrap();
        assert!(!ok);
    }
}
