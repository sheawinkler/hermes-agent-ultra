//! Track skill slugs touched during the active session (for work package binding).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::state_dir;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionSkillsFile {
    session_id: String,
    #[serde(default)]
    slugs: HashSet<String>,
    #[serde(default)]
    patch_count: u32,
    #[serde(default)]
    created: bool,
}

fn session_skills_path(hermes_home: &Path) -> PathBuf {
    state_dir(hermes_home).join("session_skills.json")
}

pub fn set_active_session(hermes_home: &Path, session_id: &str) {
    let path = session_skills_path(hermes_home);
    let mut file = SessionSkillsFile {
        session_id: session_id.to_string(),
        slugs: HashSet::new(),
        patch_count: 0,
        created: false,
    };
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(existing) = serde_json::from_str::<SessionSkillsFile>(&raw) {
            if existing.session_id == session_id {
                file = existing;
            }
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::write(path, raw);
    }
}

pub fn record_skill_touch(hermes_home: &Path, name_slug: &str, created: bool) {
    let slug = name_slug.trim();
    if slug.is_empty() {
        return;
    }
    let path = session_skills_path(hermes_home);
    let mut file = read_file(&path);
    file.slugs.insert(slug.to_string());
    if created {
        file.created = true;
    } else {
        file.patch_count = file.patch_count.saturating_add(1);
    }
    write_file(&path, &file);
}

pub fn drain_session_skills(hermes_home: &Path, session_id: &str) -> SessionSkillSummary {
    let path = session_skills_path(hermes_home);
    let file = read_file(&path);
    if file.session_id != session_id {
        return SessionSkillSummary::default();
    }
    let summary = SessionSkillSummary {
        slugs: file.slugs.into_iter().collect(),
        patch_count: file.patch_count,
        skill_created: file.created,
    };
    let _ = std::fs::remove_file(path);
    summary
}

#[derive(Debug, Clone, Default)]
pub struct SessionSkillSummary {
    pub slugs: Vec<String>,
    pub patch_count: u32,
    pub skill_created: bool,
}

fn read_file(path: &Path) -> SessionSkillsFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_file(path: &Path, file: &SessionSkillsFile) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string_pretty(file) {
        let _ = std::fs::write(path, raw);
    }
}
