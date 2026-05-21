//! Skills Hub lock file (`skills/.hub/lock.json`) — shared install provenance for runtime policy.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const HUB_STATE_DIR: &str = ".hub";
pub const HUB_LOCK_FILE: &str = "lock.json";
pub const HUB_LOCK_VERSION: u32 = 1;

/// One installed skill record from the hub lock file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillHubInstalledEntry {
    pub name: String,
    pub source: String,
    pub identifier: String,
    pub trust_level: String,
    #[serde(default)]
    pub scan_verdict: String,
    #[serde(default)]
    pub content_hash: String,
    pub install_path: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub installed_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// Hub lock file written by `hermes skills install`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SkillsHubLock {
    #[serde(default = "default_lock_version")]
    pub version: u32,
    #[serde(default)]
    pub installed: Vec<SkillHubInstalledEntry>,
}

fn default_lock_version() -> u32 {
    HUB_LOCK_VERSION
}

pub fn hub_lock_path(skills_dir: &Path) -> PathBuf {
    skills_dir.join(HUB_STATE_DIR).join(HUB_LOCK_FILE)
}

/// Read hub lock; missing or invalid file yields an empty lock.
pub fn read_hub_lock(skills_dir: &Path) -> SkillsHubLock {
    let path = hub_lock_path(skills_dir);
    let Ok(raw) = std::fs::read_to_string(path) else {
        return SkillsHubLock::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn normalize_key(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('_', "-")
}

impl SkillsHubLock {
    /// Find lock entry by installed name, frontmatter name, or skill directory folder name.
    pub fn find_entry(
        &self,
        skill_name: &str,
        skill_dir: Option<&Path>,
    ) -> Option<&SkillHubInstalledEntry> {
        let name_key = normalize_key(skill_name);
        let dir_key = skill_dir
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(normalize_key);

        self.installed.iter().find(|e| {
            let entry_key = normalize_key(&e.name);
            entry_key == name_key || dir_key.as_ref() == Some(&entry_key)
        })
    }
}

/// Source string for [`crate::skills_guard::resolve_trust_level`] / install policy.
pub fn resolve_scan_source(skills_dir: &Path, skill_name: &str, skill_dir: Option<&Path>) -> String {
    let lock = read_hub_lock(skills_dir);
    if let Some(entry) = lock.find_entry(skill_name, skill_dir) {
        return entry.identifier.clone();
    }
    skill_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_source_from_lock_by_name() {
        let tmp = TempDir::new().unwrap();
        let skills = tmp.path().join("skills");
        fs::create_dir_all(skills.join(HUB_STATE_DIR)).unwrap();
        let lock = SkillsHubLock {
            version: 1,
            installed: vec![SkillHubInstalledEntry {
                name: "my-skill".into(),
                source: "official".into(),
                identifier: "openai/skills/my-skill".into(),
                trust_level: "trusted".into(),
                scan_verdict: String::new(),
                content_hash: String::new(),
                install_path: "my-skill".into(),
                files: vec![],
                metadata: serde_json::json!({}),
                installed_at: String::new(),
                updated_at: String::new(),
            }],
        };
        fs::write(
            hub_lock_path(&skills),
            serde_json::to_string(&lock).unwrap(),
        )
        .unwrap();
        assert_eq!(
            resolve_scan_source(&skills, "my-skill", None),
            "openai/skills/my-skill"
        );
    }

    #[test]
    fn unknown_skill_defaults_to_skill_name() {
        let tmp = TempDir::new().unwrap();
        let skills = tmp.path();
        assert_eq!(resolve_scan_source(skills, "local-draft", None), "local-draft");
    }
}
