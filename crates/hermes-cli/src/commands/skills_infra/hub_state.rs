//! Skills hub lock, audit, and hashing.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use hermes_core::AgentError;
use sha2::{Digest, Sha256};

use super::constants::{SKILLS_HUB_AUDIT_FILE, SKILLS_HUB_STATE_DIR};
use super::types::{SkillHubInstalledEntry, SkillInstallProvenance, SkillsHubLockFile};

pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub(crate) fn skills_hub_state_dir(skills_dir: &Path) -> PathBuf {
    skills_dir.join(SKILLS_HUB_STATE_DIR)
}

pub(crate) fn skills_hub_lock_path(skills_dir: &Path) -> PathBuf {
    hermes_skills::hub_lock_path(skills_dir)
}

pub(crate) fn skills_hub_audit_path(skills_dir: &Path) -> PathBuf {
    skills_hub_state_dir(skills_dir).join(SKILLS_HUB_AUDIT_FILE)
}

pub(crate) fn read_skills_hub_lock(skills_dir: &Path) -> SkillsHubLockFile {
    hermes_skills::read_hub_lock(skills_dir)
}

pub(crate) fn write_skills_hub_lock(
    skills_dir: &Path,
    lock: &SkillsHubLockFile,
) -> Result<(), AgentError> {
    let state_dir = skills_hub_state_dir(skills_dir);
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create skills hub state dir '{}': {}",
            state_dir.display(),
            e
        ))
    })?;
    let path = skills_hub_lock_path(skills_dir);
    let body = serde_json::to_string_pretty(lock)
        .map_err(|e| AgentError::Config(format!("Failed to serialize skills hub lock: {}", e)))?;
    std::fs::write(&path, body).map_err(|e| {
        AgentError::Io(format!(
            "Failed to write skills hub lock '{}': {}",
            path.display(),
            e
        ))
    })
}

pub(crate) fn append_skills_hub_audit(
    skills_dir: &Path,
    action: &str,
    entry: &SkillHubInstalledEntry,
) -> Result<(), AgentError> {
    let state_dir = skills_hub_state_dir(skills_dir);
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create skills hub state dir '{}': {}",
            state_dir.display(),
            e
        ))
    })?;
    let path = skills_hub_audit_path(skills_dir);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| {
            AgentError::Io(format!(
                "Failed to open skills hub audit log '{}': {}",
                path.display(),
                e
            ))
        })?;
    let line = serde_json::json!({
        "timestamp": now_rfc3339(),
        "action": action,
        "name": entry.name,
        "source": entry.source,
        "identifier": entry.identifier,
        "trust_level": entry.trust_level,
        "scan_verdict": entry.scan_verdict,
        "content_hash": entry.content_hash,
    });
    use std::io::Write as _;
    writeln!(file, "{}", line)
        .map_err(|e| AgentError::Io(format!("Failed to append skills hub audit log: {}", e)))
}

pub(crate) fn hash_skill_bundle(files: &[(String, Bytes)]) -> String {
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel_path, bytes) in sorted {
        h.update(rel_path.as_bytes());
        h.update([0]);
        h.update(bytes.as_ref());
        h.update([0xFF]);
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

pub(crate) fn collect_skill_files_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), AgentError> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| AgentError::Io(format!("Failed to read dir '{}': {}", dir.display(), e)))?
    {
        let entry = entry.map_err(|e| {
            AgentError::Io(format!(
                "Failed to read dir entry '{}': {}",
                dir.display(),
                e
            ))
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| {
            AgentError::Io(format!(
                "Failed to get file type for '{}': {}",
                path.display(),
                e
            ))
        })?;
        if file_type.is_dir() {
            collect_skill_files_recursive(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| AgentError::Io(format!("Failed to compute relative path: {}", e)))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path)
            .map_err(|e| AgentError::Io(format!("Failed to read '{}': {}", path.display(), e)))?;
        out.push((rel, bytes));
    }
    Ok(())
}

pub(crate) fn hash_installed_skill_dir(skill_dir: &Path) -> Result<String, AgentError> {
    if !skill_dir.exists() {
        return Err(AgentError::Config(format!(
            "Installed skill path does not exist: {}",
            skill_dir.display()
        )));
    }
    let mut files = Vec::new();
    collect_skill_files_recursive(skill_dir, skill_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel_path, bytes) in files {
        h.update(rel_path.as_bytes());
        h.update([0]);
        h.update(&bytes);
        h.update([0xFF]);
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

pub(crate) fn record_skill_install_in_hub_lock(
    skills_dir: &Path,
    installed_name: &str,
    install_path: &Path,
    files: &[(String, Bytes)],
    provenance: &SkillInstallProvenance,
) -> Result<(), AgentError> {
    let mut lock = read_skills_hub_lock(skills_dir);
    let now = now_rfc3339();
    let install_path_rel = install_path
        .strip_prefix(skills_dir)
        .unwrap_or(install_path)
        .to_string_lossy()
        .replace('\\', "/");
    let content_hash = hash_installed_skill_dir(install_path)?;
    let files_rel: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let entry = SkillHubInstalledEntry {
        name: installed_name.to_string(),
        source: provenance.source.clone(),
        identifier: provenance.identifier.clone(),
        trust_level: provenance.trust_level.clone(),
        scan_verdict: "clean".to_string(),
        content_hash,
        install_path: install_path_rel,
        files: files_rel,
        metadata: provenance.metadata.clone(),
        installed_at: now.clone(),
        updated_at: now,
    };
    lock.installed.retain(|item| item.name != installed_name);
    lock.installed.push(entry.clone());
    lock.installed.sort_by(|a, b| a.name.cmp(&b.name));
    write_skills_hub_lock(skills_dir, &lock)?;
    append_skills_hub_audit(skills_dir, "INSTALL", &entry)?;
    Ok(())
}

pub(crate) fn record_skill_uninstall_in_hub_lock(
    skills_dir: &Path,
    skill_name: &str,
) -> Result<Option<SkillHubInstalledEntry>, AgentError> {
    let mut lock = read_skills_hub_lock(skills_dir);
    let mut removed: Option<SkillHubInstalledEntry> = None;
    lock.installed.retain(|entry| {
        if entry.name == skill_name {
            removed = Some(entry.clone());
            false
        } else {
            true
        }
    });
    write_skills_hub_lock(skills_dir, &lock)?;
    if let Some(ref removed_entry) = removed {
        append_skills_hub_audit(skills_dir, "UNINSTALL", removed_entry)?;
    }
    Ok(removed)
}

pub(crate) fn skills_install_force(extra: Option<&str>) -> bool {
    if extra
        .map(|e| e.split_whitespace().any(|t| t == "--force"))
        .unwrap_or(false)
    {
        return true;
    }
    std::env::var("HERMES_SKILLS_INSTALL_FORCE")
        .ok()
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

pub(crate) fn skill_guard_enforce_bundle(
    install_name: &str,
    source: &str,
    files: &[(String, Bytes)],
    force: bool,
) -> Result<(), AgentError> {
    let file_vec: Vec<(String, Vec<u8>)> =
        files.iter().map(|(p, b)| (p.clone(), b.to_vec())).collect();
    hermes_skills::SkillGuard::enforce_install_bundle(install_name, source, &file_vec, force)
        .map_err(|e| AgentError::Config(e.to_string()))
}
