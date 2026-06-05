//! Bundled and official skill synchronization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::skill::SkillError;
use crate::usage::{read_skill_name_from_dir, read_skill_name_from_file};

pub const NO_BUNDLED_SKILLS_MARKER: &str = ".no-bundled-skills";

#[derive(Debug, Clone)]
pub struct SkillSyncConfig {
    pub bundled_dir: PathBuf,
    pub optional_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub manifest_file: PathBuf,
}

impl SkillSyncConfig {
    pub fn new(bundled_dir: PathBuf, optional_dir: PathBuf, skills_dir: PathBuf) -> Self {
        let manifest_file = skills_dir.join(".bundled_manifest");
        Self {
            bundled_dir,
            optional_dir,
            skills_dir,
            manifest_file,
        }
    }

    pub fn hermes_home(&self) -> PathBuf {
        self.skills_dir
            .parent()
            .unwrap_or(self.skills_dir.as_path())
            .to_path_buf()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundledSkill {
    pub name: String,
    pub path: PathBuf,
    pub relative_dest: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSyncResult {
    pub copied: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: usize,
    pub user_modified: Vec<String>,
    pub cleaned: Vec<String>,
    pub total_bundled: usize,
    pub optional_provenance_backfilled: Vec<String>,
    #[serde(default)]
    pub collisions: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub skipped_opt_out: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillResetResult {
    pub ok: bool,
    pub action: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced: Option<SkillSyncResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficialOptionalRestoreResult {
    pub ok: bool,
    pub restored: Vec<String>,
    pub backfilled: Vec<String>,
    pub backed_up: Vec<String>,
    pub backup_dir: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundledSkillsOptOutResult {
    pub ok: bool,
    pub changed: bool,
    pub marker: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PristineBundledSkillSkip {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovePristineBundledSkillsResult {
    pub ok: bool,
    pub removed: Vec<String>,
    pub skipped: Vec<PristineBundledSkillSkip>,
    pub dry_run: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillHubInstalledEntry {
    name: String,
    source: String,
    identifier: String,
    trust_level: String,
    scan_verdict: String,
    content_hash: String,
    install_path: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    metadata: Value,
    installed_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillsHubLockFile {
    #[serde(default = "default_lock_version")]
    version: u32,
    #[serde(default)]
    installed: Vec<SkillHubInstalledEntry>,
}

fn default_lock_version() -> u32 {
    1
}

pub fn read_manifest(path: &Path) -> BTreeMap<String, String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, hash) = line
                .split_once(':')
                .map(|(name, hash)| (name.trim(), hash.trim()))
                .unwrap_or((line, ""));
            if name.is_empty() {
                None
            } else {
                Some((name.to_string(), hash.to_string()))
            }
        })
        .collect()
}

pub fn write_manifest(path: &Path, entries: &BTreeMap<String, String>) -> Result<(), SkillError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = String::new();
    for (name, hash) in entries {
        if hash.is_empty() {
            out.push_str(name);
        } else {
            out.push_str(name);
            out.push(':');
            out.push_str(hash);
        }
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(())
}

pub fn bundled_skills_opt_out_marker(hermes_home: &Path) -> PathBuf {
    hermes_home.join(NO_BUNDLED_SKILLS_MARKER)
}

pub fn is_bundled_skills_opt_out(hermes_home: &Path) -> bool {
    bundled_skills_opt_out_marker(hermes_home).exists()
}

pub fn set_bundled_skills_opt_out(
    hermes_home: &Path,
    enabled: bool,
) -> Result<BundledSkillsOptOutResult, SkillError> {
    let marker = bundled_skills_opt_out_marker(hermes_home);
    let existed = marker.exists();
    if enabled {
        fs::create_dir_all(hermes_home)?;
        fs::write(
            &marker,
            "This profile opted out of bundled-skill seeding (`hermes skills opt-out`).\n\
             Delete this file to re-enable sync on the next `hermes update`.\n",
        )?;
        let changed = !existed;
        let message = if changed {
            "Opted out of bundled skills. Future install / update / sync runs will not seed bundled skills into this profile."
        } else {
            "Already opted out - marker was already present."
        };
        return Ok(BundledSkillsOptOutResult {
            ok: true,
            changed,
            marker: marker.to_string_lossy().to_string(),
            message: message.to_string(),
        });
    }

    if existed {
        fs::remove_file(&marker)?;
    }
    let message = if existed {
        "Opted back in. The next `hermes update` (or `hermes skills opt-in --sync`) will re-seed bundled skills."
    } else {
        "Not opted out - no marker to remove."
    };
    Ok(BundledSkillsOptOutResult {
        ok: true,
        changed: existed,
        marker: marker.to_string_lossy().to_string(),
        message: message.to_string(),
    })
}

pub fn dir_hash(dir: &Path) -> String {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut ctx = md5::Context::new();
    for (rel, path) in files {
        ctx.consume(rel.as_bytes());
        ctx.consume([0]);
        if let Ok(bytes) = fs::read(path) {
            ctx.consume(bytes);
        }
        ctx.consume([0xff]);
    }
    format!("{:x}", ctx.compute())
}

fn collect_files(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if name == ".git" || name.starts_with(".DS_Store") {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, files);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            files.push((rel, path));
        }
    }
}

pub fn read_skill_name(skill_md: &Path, fallback: &str) -> String {
    read_skill_name_from_file(skill_md, fallback)
}

pub fn compute_relative_dest(skill_dir: &Path, bundled_dir: &Path) -> PathBuf {
    skill_dir
        .strip_prefix(bundled_dir)
        .unwrap_or(skill_dir)
        .to_path_buf()
}

pub fn discover_bundled_skills(bundled_dir: &Path) -> Vec<BundledSkill> {
    let mut out = Vec::new();
    if !bundled_dir.exists() {
        return out;
    }
    let mut stack = vec![bundled_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if name.starts_with('.') || name == "node_modules" || name == "__pycache__" {
                continue;
            }
            if !path.is_dir() {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                out.push(BundledSkill {
                    name: read_skill_name(&skill_md, name),
                    relative_dest: compute_relative_dest(&path, bundled_dir),
                    path,
                });
            } else {
                stack.push(path);
            }
        }
    }
    out.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.relative_dest.cmp(&b.relative_dest))
    });
    out
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), SkillError> {
    fs::create_dir_all(dst)?;
    let entries = fs::read_dir(src)?;
    for entry in entries.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if src_path.is_file() {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn replace_dir_atomic(src: &Path, dst: &Path) -> Result<(), SkillError> {
    let parent = dst.parent().ok_or_else(|| {
        SkillError::Io(format!("Missing destination parent for {}", dst.display()))
    })?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.sync-{}",
        dst.file_name().and_then(|v| v.to_str()).unwrap_or("skill"),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    copy_dir_recursive(src, &staging)?;
    if dst.exists() {
        let backup = parent.join(format!(
            ".{}.backup-{}",
            dst.file_name().and_then(|v| v.to_str()).unwrap_or("skill"),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::rename(dst, &backup)?;
        match fs::rename(&staging, dst) {
            Ok(()) => {
                let _ = fs::remove_dir_all(backup);
            }
            Err(err) => {
                let _ = fs::rename(&backup, dst);
                let _ = fs::remove_dir_all(&staging);
                return Err(err.into());
            }
        }
    } else {
        fs::rename(&staging, dst)?;
    }
    Ok(())
}

fn copy_parent_descriptions(
    skill: &BundledSkill,
    config: &SkillSyncConfig,
) -> Result<(), SkillError> {
    let rel_parent = skill
        .relative_dest
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut current = PathBuf::new();
    for component in rel_parent.components() {
        current.push(component.as_os_str());
        let src = config.bundled_dir.join(&current).join("DESCRIPTION.md");
        if src.exists() {
            let dst = config.skills_dir.join(&current).join("DESCRIPTION.md");
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            if !dst.exists() {
                fs::copy(src, dst)?;
            }
        }
    }
    Ok(())
}

pub fn sync_skills(config: &SkillSyncConfig, quiet: bool) -> Result<SkillSyncResult, SkillError> {
    if is_bundled_skills_opt_out(&config.hermes_home()) {
        if !quiet {
            println!(
                "  (skipped - profile opted out of bundled skills via {})",
                NO_BUNDLED_SKILLS_MARKER
            );
        }
        return Ok(SkillSyncResult {
            skipped_opt_out: true,
            ..Default::default()
        });
    }

    if !config.bundled_dir.exists() {
        return Ok(SkillSyncResult::default());
    }
    fs::create_dir_all(&config.skills_dir)?;
    let mut result = SkillSyncResult::default();
    let mut manifest = read_manifest(&config.manifest_file);
    let bundled = discover_bundled_skills(&config.bundled_dir);
    result.total_bundled = bundled.len();

    let bundled_names: BTreeSet<String> = bundled.iter().map(|s| s.name.clone()).collect();
    let stale: Vec<String> = manifest
        .keys()
        .filter(|name| !bundled_names.contains(*name))
        .cloned()
        .collect();
    for name in stale {
        manifest.remove(&name);
        result.cleaned.push(name);
    }

    for skill in &bundled {
        let bundled_hash = dir_hash(&skill.path);
        let dest = config.skills_dir.join(&skill.relative_dest);
        let manifest_hash = manifest.get(&skill.name).cloned();

        if let Some(origin_hash) = manifest_hash {
            if !dest.exists() {
                result.skipped += 1;
                continue;
            }
            let user_hash = dir_hash(&dest);
            if origin_hash.is_empty() {
                manifest.insert(skill.name.clone(), user_hash);
                result.skipped += 1;
            } else if user_hash == origin_hash {
                if user_hash == bundled_hash {
                    result.skipped += 1;
                } else {
                    match replace_dir_atomic(&skill.path, &dest) {
                        Ok(()) => {
                            manifest.insert(skill.name.clone(), bundled_hash);
                            result.updated.push(skill.name.clone());
                            copy_parent_descriptions(skill, config)?;
                        }
                        Err(err) => result
                            .errors
                            .push(format!("Failed to update {}: {}", skill.name, err)),
                    }
                }
            } else {
                result.user_modified.push(skill.name.clone());
            }
            continue;
        }

        if dest.exists() {
            result.collisions.push(skill.name.clone());
            if !quiet {
                println!(
                    "Bundled skill '{}' is shadowed by an existing local copy. Run `hermes skills reset {}` to accept bundled.",
                    skill.name, skill.name
                );
            }
            continue;
        }

        match copy_dir_recursive(&skill.path, &dest) {
            Ok(()) => {
                manifest.insert(skill.name.clone(), bundled_hash);
                result.copied.push(skill.name.clone());
                copy_parent_descriptions(skill, config)?;
            }
            Err(err) => result
                .errors
                .push(format!("Failed to copy {}: {}", skill.name, err)),
        }
    }

    let backfilled = backfill_optional_provenance(config)?;
    result.optional_provenance_backfilled = backfilled;
    write_manifest(&config.manifest_file, &manifest)?;
    result.copied.sort();
    result.updated.sort();
    result.user_modified.sort();
    result.cleaned.sort();
    result.collisions.sort();
    Ok(result)
}

pub fn remove_pristine_bundled_skills(
    config: &SkillSyncConfig,
    dry_run: bool,
) -> Result<RemovePristineBundledSkillsResult, SkillError> {
    let mut manifest = read_manifest(&config.manifest_file);
    let bundled_by_name: BTreeMap<String, BundledSkill> =
        discover_bundled_skills(&config.bundled_dir)
            .into_iter()
            .map(|skill| (skill.name.clone(), skill))
            .collect();

    let mut result = RemovePristineBundledSkillsResult {
        ok: true,
        dry_run,
        ..Default::default()
    };
    let mut manifest_changed = false;

    for (name, origin_hash) in manifest.clone() {
        let Some(skill) = bundled_by_name.get(&name) else {
            result.skipped.push(PristineBundledSkillSkip {
                name,
                reason: "no bundled source (removed upstream)".to_string(),
            });
            continue;
        };
        let dest = config.skills_dir.join(&skill.relative_dest);
        if !dest.exists() {
            if !dry_run {
                manifest.remove(&name);
                manifest_changed = true;
            }
            continue;
        }
        if origin_hash.is_empty() {
            result.skipped.push(PristineBundledSkillSkip {
                name,
                reason: "legacy manifest without origin hash (kept)".to_string(),
            });
            continue;
        }
        let on_disk = dir_hash(&dest);
        if on_disk != origin_hash {
            result.skipped.push(PristineBundledSkillSkip {
                name,
                reason: "user-modified (kept)".to_string(),
            });
            continue;
        }

        if dry_run {
            result.removed.push(name);
            continue;
        }

        match fs::remove_dir_all(&dest) {
            Ok(()) => {
                manifest.remove(&name);
                manifest_changed = true;
                result.removed.push(name);
            }
            Err(err) => result.skipped.push(PristineBundledSkillSkip {
                name,
                reason: format!("delete failed: {err}"),
            }),
        }
    }

    if manifest_changed {
        write_manifest(&config.manifest_file, &manifest)?;
    }

    result.removed.sort();
    result.skipped.sort_by(|a, b| a.name.cmp(&b.name));
    let verb = if dry_run { "Would remove" } else { "Removed" };
    result.message = format!(
        "{} {} pristine bundled skill(s); kept {}.",
        verb,
        result.removed.len(),
        result.skipped.len()
    );
    Ok(result)
}

fn skills_hub_lock_path(skills_dir: &Path) -> PathBuf {
    skills_dir.join(".hub").join("lock.json")
}

fn read_hub_lock(skills_dir: &Path) -> SkillsHubLockFile {
    let Ok(raw) = fs::read_to_string(skills_hub_lock_path(skills_dir)) else {
        return SkillsHubLockFile {
            version: 1,
            installed: Vec::new(),
        };
    };
    serde_json::from_str::<SkillsHubLockFile>(&raw).unwrap_or(SkillsHubLockFile {
        version: 1,
        installed: Vec::new(),
    })
}

fn write_hub_lock(skills_dir: &Path, lock: &SkillsHubLockFile) -> Result<(), SkillError> {
    let path = skills_hub_lock_path(skills_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(lock)
        .map_err(|e| SkillError::Parse(format!("Failed to encode hub lock: {e}")))?;
    fs::write(path, raw)?;
    Ok(())
}

fn record_official_lock(
    config: &SkillSyncConfig,
    name: &str,
    rel_dest: &Path,
    skill_dir: &Path,
) -> Result<(), SkillError> {
    let mut lock = read_hub_lock(&config.skills_dir);
    let install_path = rel_dest.to_string_lossy().replace('\\', "/");
    let now = Utc::now().to_rfc3339();
    lock.installed.retain(|entry| entry.name != name);
    lock.installed.push(SkillHubInstalledEntry {
        name: name.to_string(),
        source: "official".to_string(),
        identifier: format!("official/{install_path}"),
        trust_level: "builtin".to_string(),
        scan_verdict: "clean".to_string(),
        content_hash: format!("md5:{}", dir_hash(skill_dir)),
        install_path,
        files: Vec::new(),
        metadata: Value::Null,
        installed_at: now.clone(),
        updated_at: now,
    });
    lock.installed.sort_by(|a, b| a.name.cmp(&b.name));
    write_hub_lock(&config.skills_dir, &lock)
}

fn discover_optional_skills(optional_dir: &Path) -> Vec<BundledSkill> {
    discover_bundled_skills(optional_dir)
}

fn backfill_optional_provenance(config: &SkillSyncConfig) -> Result<Vec<String>, SkillError> {
    let mut out = Vec::new();
    if !config.optional_dir.exists() {
        return Ok(out);
    }
    let lock = read_hub_lock(&config.skills_dir);
    let existing: BTreeSet<String> = lock.installed.into_iter().map(|entry| entry.name).collect();
    for skill in discover_optional_skills(&config.optional_dir) {
        if existing.contains(&skill.name) {
            continue;
        }
        let active = config.skills_dir.join(&skill.relative_dest);
        if active.exists() && dir_hash(&active) == dir_hash(&skill.path) {
            record_official_lock(config, &skill.name, &skill.relative_dest, &active)?;
            out.push(
                skill
                    .relative_dest
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or(skill.name.as_str())
                    .to_string(),
            );
        }
    }
    out.sort();
    Ok(out)
}

fn find_bundled_by_name(root: &Path, name: &str) -> Option<BundledSkill> {
    discover_bundled_skills(root).into_iter().find(|skill| {
        skill.name == name || skill.relative_dest.file_name().and_then(|v| v.to_str()) == Some(name)
    })
}

pub fn reset_bundled_skill(
    config: &SkillSyncConfig,
    skill_name: &str,
    restore: bool,
) -> Result<SkillResetResult, SkillError> {
    let name = skill_name.trim();
    let mut manifest = read_manifest(&config.manifest_file);
    let Some(skill) = find_bundled_by_name(&config.bundled_dir, name) else {
        return Ok(SkillResetResult {
            ok: false,
            action: "not_in_manifest".to_string(),
            message: format!("Skill '{name}' is not a tracked bundled skill."),
            synced: None,
        });
    };
    let dest = config.skills_dir.join(&skill.relative_dest);
    if restore {
        if dest.exists() {
            if let Err(err) = fs::remove_dir_all(&dest) {
                return Ok(SkillResetResult {
                    ok: false,
                    action: "not_reset".to_string(),
                    message: format!(
                        "Failed to remove '{}': {}. Manifest entry preserved.",
                        dest.display(),
                        err
                    ),
                    synced: None,
                });
            }
        }
        manifest.remove(&skill.name);
        write_manifest(&config.manifest_file, &manifest)?;
        let synced = sync_skills(config, true)?;
        return Ok(SkillResetResult {
            ok: true,
            action: "restored".to_string(),
            message: format!("Skill '{}' restored from bundled copy.", skill.name),
            synced: Some(synced),
        });
    }

    if !manifest.contains_key(&skill.name) && !dest.exists() {
        return Ok(SkillResetResult {
            ok: false,
            action: "not_in_manifest".to_string(),
            message: format!("Skill '{name}' is not a tracked bundled skill."),
            synced: None,
        });
    }
    manifest.insert(skill.name.clone(), dir_hash(&skill.path));
    write_manifest(&config.manifest_file, &manifest)?;
    Ok(SkillResetResult {
        ok: true,
        action: "manifest_cleared".to_string(),
        message: format!("Skill '{}' re-baselined to bundled hash.", skill.name),
        synced: None,
    })
}

fn find_active_skill_dirs_by_frontmatter(skills_dir: &Path, declared_name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
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
            if name.starts_with('.') || !path.is_dir() {
                continue;
            }
            if path.join("SKILL.md").exists() {
                if read_skill_name_from_dir(&path) == declared_name {
                    out.push(path);
                }
            } else {
                stack.push(path);
            }
        }
    }
    out
}

pub fn restore_official_optional_skill(
    config: &SkillSyncConfig,
    skill_name: &str,
    restore: bool,
) -> Result<OfficialOptionalRestoreResult, SkillError> {
    let Some(skill) = discover_optional_skills(&config.optional_dir)
        .into_iter()
        .find(|skill| {
            skill.name == skill_name
                || skill.relative_dest.file_name().and_then(|v| v.to_str()) == Some(skill_name)
        })
    else {
        return Ok(OfficialOptionalRestoreResult {
            ok: false,
            message: format!("Official optional skill '{skill_name}' not found."),
            ..Default::default()
        });
    };

    let canonical = config.skills_dir.join(&skill.relative_dest);
    let mut result = OfficialOptionalRestoreResult {
        ok: true,
        backup_dir: config
            .skills_dir
            .join(".archive")
            .join(format!(
                "official-restore-{}",
                Utc::now().format("%Y%m%d%H%M%S")
            ))
            .to_string_lossy()
            .to_string(),
        ..Default::default()
    };

    if canonical.exists() && dir_hash(&canonical) == dir_hash(&skill.path) {
        record_official_lock(config, &skill.name, &skill.relative_dest, &canonical)?;
        result.backfilled.push(
            skill
                .relative_dest
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or(skill.name.as_str())
                .to_string(),
        );
        return Ok(result);
    }

    if !restore {
        return Ok(result);
    }

    let backup_root = PathBuf::from(&result.backup_dir);
    let canonical_rel = skill.relative_dest.to_string_lossy().replace('\\', "/");
    for active in find_active_skill_dirs_by_frontmatter(&config.skills_dir, &skill.name) {
        if active == canonical {
            continue;
        }
        let rel = active
            .strip_prefix(&config.skills_dir)
            .unwrap_or(&active)
            .to_path_buf();
        let backup_dest = backup_root.join(&rel);
        if let Some(parent) = backup_dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&active, &backup_dest)?;
        result
            .backed_up
            .push(rel.to_string_lossy().replace('\\', "/"));
    }

    if canonical.exists() {
        let backup_dest = backup_root.join(&skill.relative_dest);
        if let Some(parent) = backup_dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&canonical, &backup_dest)?;
        result.backed_up.push(canonical_rel.clone());
    }
    copy_dir_recursive(&skill.path, &canonical)?;
    record_official_lock(config, &skill.name, &skill.relative_dest, &canonical)?;
    result.restored.push(
        skill
            .relative_dest
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or(skill.name.as_str())
            .to_string(),
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_bundled(root: &Path) -> PathBuf {
        let bundled = root.join("bundled");
        let new_skill = bundled.join("category").join("new-skill");
        fs::create_dir_all(&new_skill).unwrap();
        fs::write(new_skill.join("SKILL.md"), "# New").unwrap();
        fs::write(new_skill.join("main.rs"), "fn main() {}").unwrap();
        fs::write(bundled.join("category").join("DESCRIPTION.md"), "Category").unwrap();
        let old_skill = bundled.join("old-skill");
        fs::create_dir_all(&old_skill).unwrap();
        fs::write(old_skill.join("SKILL.md"), "# Old").unwrap();
        bundled
    }

    fn config(root: &Path, bundled: PathBuf) -> SkillSyncConfig {
        let skills_dir = root.join("skills");
        SkillSyncConfig::new(bundled, root.join("optional-skills"), skills_dir)
    }

    #[test]
    fn manifest_read_write_v1_v2_and_sorted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");
        fs::write(&path, "old-skill\nnew-skill:abc\n\n").unwrap();
        let data = read_manifest(&path);
        assert_eq!(data.get("old-skill").map(String::as_str), Some(""));
        assert_eq!(data.get("new-skill").map(String::as_str), Some("abc"));
        let mut entries = BTreeMap::new();
        entries.insert("zebra".to_string(), "1".to_string());
        entries.insert("alpha".to_string(), "2".to_string());
        write_manifest(&path, &entries).unwrap();
        assert!(fs::read_to_string(path)
            .unwrap()
            .starts_with("alpha:2\nzebra:1"));
    }

    #[test]
    fn dir_hash_is_stable_and_sensitive() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        for path in [&a, &b] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("SKILL.md"), "# Skill").unwrap();
        }
        assert_eq!(dir_hash(&a), dir_hash(&b));
        fs::write(b.join("extra.md"), "x").unwrap();
        assert_ne!(dir_hash(&a), dir_hash(&b));
        assert_eq!(dir_hash(&dir.path().join("missing")).len(), 32);
    }

    #[test]
    fn discover_uses_frontmatter_and_ignores_hidden_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("mlops").join("axolotl")).unwrap();
        fs::write(
            root.join("mlops").join("axolotl").join("SKILL.md"),
            "---\nname: axolotl-skill\n---\n# body",
        )
        .unwrap();
        fs::create_dir_all(root.join(".git").join("fake")).unwrap();
        fs::write(root.join(".git").join("fake").join("SKILL.md"), "# fake").unwrap();
        let skills = discover_bundled_skills(root);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "axolotl-skill");
        assert_eq!(
            compute_relative_dest(&skills[0].path, root),
            PathBuf::from("mlops/axolotl")
        );
    }

    #[test]
    fn fresh_sync_copies_records_hashes_and_category_description() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled);
        let result = sync_skills(&cfg, true).unwrap();
        assert_eq!(result.total_bundled, 2);
        assert_eq!(result.copied, vec!["new-skill", "old-skill"]);
        assert!(cfg.skills_dir.join("category/new-skill/SKILL.md").exists());
        assert!(cfg.skills_dir.join("category/DESCRIPTION.md").exists());
        let manifest = read_manifest(&cfg.manifest_file);
        assert_eq!(manifest["new-skill"].len(), 32);
    }

    #[test]
    fn sync_skills_honors_bundled_skills_opt_out_marker() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled);
        set_bundled_skills_opt_out(dir.path(), true).unwrap();

        let result = sync_skills(&cfg, true).unwrap();

        assert!(result.skipped_opt_out);
        assert_eq!(result.total_bundled, 0);
        assert!(!cfg.skills_dir.join("category/new-skill/SKILL.md").exists());
        assert!(is_bundled_skills_opt_out(dir.path()));

        let disabled = set_bundled_skills_opt_out(dir.path(), false).unwrap();
        assert!(disabled.changed);
        assert!(!is_bundled_skills_opt_out(dir.path()));
        let synced = sync_skills(&cfg, true).unwrap();
        assert_eq!(synced.copied, vec!["new-skill", "old-skill"]);
    }

    #[test]
    fn opt_out_marker_toggle_is_idempotent() {
        let dir = tempdir().unwrap();
        let first = set_bundled_skills_opt_out(dir.path(), true).unwrap();
        let second = set_bundled_skills_opt_out(dir.path(), true).unwrap();
        assert!(first.ok);
        assert!(first.changed);
        assert!(!second.changed);
        assert!(PathBuf::from(&second.marker).exists());

        let third = set_bundled_skills_opt_out(dir.path(), false).unwrap();
        let fourth = set_bundled_skills_opt_out(dir.path(), false).unwrap();
        assert!(third.changed);
        assert!(!fourth.changed);
        assert!(!PathBuf::from(&fourth.marker).exists());
    }

    #[test]
    fn sync_updates_unmodified_and_preserves_user_modified() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled.clone());
        fs::create_dir_all(cfg.skills_dir.join("old-skill")).unwrap();
        fs::write(cfg.skills_dir.join("old-skill/SKILL.md"), "# Old v1").unwrap();
        let old_hash = dir_hash(&cfg.skills_dir.join("old-skill"));
        fs::create_dir_all(&cfg.skills_dir).unwrap();
        fs::write(&cfg.manifest_file, format!("old-skill:{old_hash}\n")).unwrap();
        let result = sync_skills(&cfg, true).unwrap();
        assert_eq!(result.updated, vec!["old-skill"]);
        assert_eq!(
            fs::read_to_string(cfg.skills_dir.join("old-skill/SKILL.md")).unwrap(),
            "# Old"
        );

        fs::write(cfg.skills_dir.join("old-skill/SKILL.md"), "# My custom").unwrap();
        let result = sync_skills(&cfg, true).unwrap();
        assert_eq!(result.user_modified, vec!["old-skill"]);
        assert_eq!(
            fs::read_to_string(cfg.skills_dir.join("old-skill/SKILL.md")).unwrap(),
            "# My custom"
        );
    }

    #[test]
    fn collision_does_not_poison_manifest_or_flag_second_sync() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled);
        let dest = cfg.skills_dir.join("category/new-skill");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("SKILL.md"), "# unrelated").unwrap();
        let first = sync_skills(&cfg, true).unwrap();
        assert_eq!(first.collisions, vec!["new-skill"]);
        assert!(!read_manifest(&cfg.manifest_file).contains_key("new-skill"));
        let second = sync_skills(&cfg, true).unwrap();
        assert!(!second.user_modified.contains(&"new-skill".to_string()));
    }

    #[test]
    fn remove_pristine_bundled_skills_keeps_modified_and_local_then_opt_in_reseeds() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled);
        sync_skills(&cfg, true).unwrap();
        fs::write(cfg.skills_dir.join("old-skill/SKILL.md"), "# customized").unwrap();
        let local = cfg.skills_dir.join("local-only");
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("SKILL.md"), "# Local").unwrap();
        set_bundled_skills_opt_out(dir.path(), true).unwrap();

        let dry_run = remove_pristine_bundled_skills(&cfg, true).unwrap();
        assert_eq!(dry_run.removed, vec!["new-skill"]);
        assert_eq!(dry_run.skipped[0].name, "old-skill");
        assert!(cfg.skills_dir.join("category/new-skill/SKILL.md").exists());

        let removed = remove_pristine_bundled_skills(&cfg, false).unwrap();
        assert_eq!(removed.removed, vec!["new-skill"]);
        assert!(!cfg.skills_dir.join("category/new-skill").exists());
        assert!(cfg.skills_dir.join("old-skill/SKILL.md").exists());
        assert!(cfg.skills_dir.join("local-only/SKILL.md").exists());
        let manifest = read_manifest(&cfg.manifest_file);
        assert!(!manifest.contains_key("new-skill"));
        assert!(manifest.contains_key("old-skill"));

        assert!(sync_skills(&cfg, true).unwrap().skipped_opt_out);
        set_bundled_skills_opt_out(dir.path(), false).unwrap();
        let reseeded = sync_skills(&cfg, true).unwrap();
        assert_eq!(reseeded.copied, vec!["new-skill"]);
        assert!(cfg.skills_dir.join("category/new-skill/SKILL.md").exists());
        assert_eq!(
            fs::read_to_string(cfg.skills_dir.join("old-skill/SKILL.md")).unwrap(),
            "# customized"
        );
    }

    #[test]
    fn remove_pristine_bundled_skills_cleans_missing_manifest_entries() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled);
        sync_skills(&cfg, true).unwrap();
        fs::remove_dir_all(cfg.skills_dir.join("old-skill")).unwrap();

        let result = remove_pristine_bundled_skills(&cfg, false).unwrap();

        assert_eq!(result.removed, vec!["new-skill"]);
        let manifest = read_manifest(&cfg.manifest_file);
        assert!(!manifest.contains_key("old-skill"));
        assert!(!manifest.contains_key("new-skill"));
    }

    #[test]
    fn v1_manifest_migrates_to_user_baseline_then_detects_update() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled.clone());
        fs::create_dir_all(cfg.skills_dir.join("old-skill")).unwrap();
        fs::write(cfg.skills_dir.join("old-skill/SKILL.md"), "# Old").unwrap();
        fs::write(&cfg.manifest_file, "old-skill\n").unwrap();
        sync_skills(&cfg, true).unwrap();
        assert_eq!(read_manifest(&cfg.manifest_file)["old-skill"].len(), 32);
        fs::write(bundled.join("old-skill/SKILL.md"), "# Old v2").unwrap();
        let result = sync_skills(&cfg, true).unwrap();
        assert_eq!(result.updated, vec!["old-skill"]);
    }

    #[test]
    fn reset_rebaselines_or_restores_without_manifest_limbo() {
        let dir = tempdir().unwrap();
        let bundled = dir.path().join("bundled");
        let skill = bundled.join("productivity/google-workspace");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: google-workspace\n---\n# upstream\n",
        )
        .unwrap();
        let cfg = config(dir.path(), bundled);
        let dest = cfg.skills_dir.join("productivity/google-workspace");
        fs::create_dir_all(&dest).unwrap();
        fs::write(
            dest.join("SKILL.md"),
            "---\nname: google-workspace\n---\n# upstream\n",
        )
        .unwrap();
        fs::write(&cfg.manifest_file, "google-workspace:STALE\n").unwrap();
        assert_eq!(
            sync_skills(&cfg, true).unwrap().user_modified,
            vec!["google-workspace"]
        );
        let reset = reset_bundled_skill(&cfg, "google-workspace", false).unwrap();
        assert!(reset.ok);
        assert_eq!(reset.action, "manifest_cleared");
        let restored = reset_bundled_skill(&cfg, "google-workspace", true).unwrap();
        assert!(restored.ok);
        assert_eq!(restored.action, "restored");
        assert!(restored
            .synced
            .unwrap()
            .copied
            .contains(&"google-workspace".to_string()));
    }

    #[test]
    fn optional_provenance_backfill_and_restore_with_backup() {
        let dir = tempdir().unwrap();
        let bundled = setup_bundled(dir.path());
        let cfg = config(dir.path(), bundled);
        let optional = cfg.optional_dir.join("mlops/training/trl-fine-tuning");
        fs::create_dir_all(&optional).unwrap();
        fs::write(
            optional.join("SKILL.md"),
            "---\nname: fine-tuning-with-trl\n---\n# official\n",
        )
        .unwrap();
        let active = cfg.skills_dir.join("mlops/training/trl-fine-tuning");
        fs::create_dir_all(&active).unwrap();
        fs::write(
            active.join("SKILL.md"),
            "---\nname: fine-tuning-with-trl\n---\n# official\n",
        )
        .unwrap();
        let result = sync_skills(&cfg, true).unwrap();
        assert_eq!(
            result.optional_provenance_backfilled,
            vec!["trl-fine-tuning"]
        );

        let wrong = cfg.skills_dir.join("mlops/trl-fine-tuning");
        fs::create_dir_all(&wrong).unwrap();
        fs::write(
            wrong.join("SKILL.md"),
            "---\nname: fine-tuning-with-trl\n---\n# wrong\n",
        )
        .unwrap();
        fs::write(active.join("SKILL.md"), "# modified\n").unwrap();
        let repair = restore_official_optional_skill(&cfg, "fine-tuning-with-trl", true).unwrap();
        assert!(repair.ok);
        assert_eq!(repair.restored, vec!["trl-fine-tuning"]);
        assert!(repair
            .backed_up
            .contains(&"mlops/trl-fine-tuning".to_string()));
        assert!(fs::read_to_string(active.join("SKILL.md"))
            .unwrap()
            .contains("official"));
    }
}
