//! Consent-first suggested cron jobs.
//!
//! Suggestions are ready-to-run cron job specs that become real jobs only when
//! the user accepts them. The scheduler remains the single execution engine.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::blueprints::{catalog as blueprint_catalog, fill_blueprint};

pub const MAX_PENDING_SUGGESTIONS: usize = 5;

const VALID_SOURCES: &[&str] = &["catalog", "blueprint", "usage", "integration"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionStatus {
    Pending,
    Accepted,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionJobSpec {
    pub schedule: String,
    pub prompt: String,
    pub name: String,
    #[serde(default = "default_deliver")]
    pub deliver: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

fn default_deliver() -> String {
    "origin".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionRecord {
    pub id: String,
    pub title: String,
    pub description: String,
    pub source: String,
    pub job_spec: SuggestionJobSpec,
    pub dedup_key: String,
    pub status: SuggestionStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SuggestionFile {
    #[serde(default)]
    suggestions: Vec<SuggestionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SuggestionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Corrupted suggestions data: {0}")]
    Corrupted(String),

    #[error("Invalid suggestion: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct SuggestionStore {
    path: PathBuf,
}

impl SuggestionStore {
    pub fn new() -> Self {
        Self {
            path: hermes_config::cron_dir().join("suggestions.json"),
        }
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn load_suggestions(&self) -> Result<Vec<SuggestionRecord>, SuggestionError> {
        with_suggestion_lock(|| self.load_suggestions_unlocked())
    }

    pub fn list_pending(&self) -> Result<Vec<SuggestionRecord>, SuggestionError> {
        let records = self.load_suggestions()?;
        Ok(records
            .into_iter()
            .filter(|record| record.status == SuggestionStatus::Pending)
            .collect())
    }

    pub fn seed_catalog_suggestions(&self) -> Result<Vec<SuggestionRecord>, SuggestionError> {
        with_suggestion_lock(|| {
            let mut records = self.load_suggestions_unlocked()?;
            let mut created = Vec::new();
            for blueprint in blueprint_catalog() {
                let spec = fill_blueprint(&blueprint, &BTreeMap::new()).map_err(|err| {
                    SuggestionError::Invalid(format!(
                        "blueprint {} default fill failed: {err}",
                        blueprint.key
                    ))
                })?;
                let input = AddSuggestionInput {
                    title: blueprint.title.to_string(),
                    description: blueprint.description.to_string(),
                    source: "catalog".to_string(),
                    job_spec: SuggestionJobSpec {
                        schedule: spec.schedule,
                        prompt: spec.prompt,
                        name: spec.title,
                        deliver: spec.deliver,
                        skills: spec.skills,
                    },
                    dedup_key: format!("catalog:{}", blueprint.key),
                };
                if let Some(record) = add_suggestion_to_records(&mut records, input)? {
                    created.push(record);
                }
            }
            if !created.is_empty() {
                self.save_suggestions_unlocked(&records)?;
            }
            Ok(created)
        })
    }

    pub fn get_pending(
        &self,
        reference: &str,
    ) -> Result<Option<SuggestionRecord>, SuggestionError> {
        let records = self.load_suggestions()?;
        Ok(resolve_suggestion(&records, reference)
            .filter(|record| record.status == SuggestionStatus::Pending)
            .cloned())
    }

    pub fn dismiss_suggestion(&self, reference: &str) -> Result<bool, SuggestionError> {
        with_suggestion_lock(|| {
            let mut records = self.load_suggestions_unlocked()?;
            let Some(record) = resolve_suggestion(&records, reference) else {
                return Ok(false);
            };
            if record.status != SuggestionStatus::Pending {
                return Ok(false);
            }
            let id = record.id.clone();
            let changed = set_status(&mut records, &id, SuggestionStatus::Dismissed);
            if changed {
                self.save_suggestions_unlocked(&records)?;
            }
            Ok(changed)
        })
    }

    pub fn mark_accepted(&self, suggestion_id: &str) -> Result<bool, SuggestionError> {
        with_suggestion_lock(|| {
            let mut records = self.load_suggestions_unlocked()?;
            let Some(record) = records
                .iter()
                .find(|record| record.id.as_str() == suggestion_id)
            else {
                return Ok(false);
            };
            if record.status != SuggestionStatus::Pending {
                return Ok(false);
            }
            let changed = set_status(&mut records, suggestion_id, SuggestionStatus::Accepted);
            if changed {
                self.save_suggestions_unlocked(&records)?;
            }
            Ok(changed)
        })
    }

    pub fn clear_resolved(&self) -> Result<usize, SuggestionError> {
        with_suggestion_lock(|| {
            let mut records = self.load_suggestions_unlocked()?;
            let before = records.len();
            records.retain(|record| record.status != SuggestionStatus::Accepted);
            let removed = before - records.len();
            if removed > 0 {
                self.save_suggestions_unlocked(&records)?;
            }
            Ok(removed)
        })
    }

    fn load_suggestions_unlocked(&self) -> Result<Vec<SuggestionRecord>, SuggestionError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        if value.is_array() {
            return serde_json::from_value::<Vec<SuggestionRecord>>(value)
                .map_err(SuggestionError::Serialization);
        }
        let file = serde_json::from_value::<SuggestionFile>(value)?;
        Ok(file.suggestions)
    }

    fn save_suggestions_unlocked(
        &self,
        records: &[SuggestionRecord],
    ) -> Result<(), SuggestionError> {
        let Some(parent) = self.path.parent() else {
            return Err(SuggestionError::Corrupted(format!(
                "suggestions path has no parent: {}",
                self.path.display()
            )));
        };
        std::fs::create_dir_all(parent)?;
        secure_dir(parent)?;

        let file = SuggestionFile {
            suggestions: records.to_vec(),
            updated_at: Some(Utc::now()),
        };
        let raw = serde_json::to_string_pretty(&file)?;
        let tmp = parent.join(format!(
            ".suggestions.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let write_result = (|| -> Result<(), SuggestionError> {
            let mut handle = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp)?;
            handle.write_all(raw.as_bytes())?;
            handle.flush()?;
            handle.sync_all()?;
            secure_file(&tmp)?;
            std::fs::rename(&tmp, &self.path)?;
            secure_file(&self.path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        write_result
    }
}

impl Default for SuggestionStore {
    fn default() -> Self {
        Self::new()
    }
}

struct AddSuggestionInput {
    title: String,
    description: String,
    source: String,
    job_spec: SuggestionJobSpec,
    dedup_key: String,
}

fn add_suggestion_to_records(
    records: &mut Vec<SuggestionRecord>,
    input: AddSuggestionInput,
) -> Result<Option<SuggestionRecord>, SuggestionError> {
    let title = input.title.trim();
    let source = input.source.trim();
    let dedup_key = input.dedup_key.trim();
    if title.is_empty() || dedup_key.is_empty() {
        return Err(SuggestionError::Invalid(
            "title and dedup_key are required".to_string(),
        ));
    }
    if !VALID_SOURCES.contains(&source) {
        return Err(SuggestionError::Invalid(format!(
            "unknown suggestion source: {source}"
        )));
    }
    if records
        .iter()
        .any(|record| record.dedup_key.as_str() == dedup_key)
    {
        return Ok(None);
    }
    let pending_count = records
        .iter()
        .filter(|record| record.status == SuggestionStatus::Pending)
        .count();
    if pending_count >= MAX_PENDING_SUGGESTIONS {
        return Ok(None);
    }

    let record = SuggestionRecord {
        id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
        title: title.to_string(),
        description: input.description.trim().to_string(),
        source: source.to_string(),
        job_spec: input.job_spec,
        dedup_key: dedup_key.to_string(),
        status: SuggestionStatus::Pending,
        created_at: Utc::now(),
        resolved_at: None,
    };
    records.push(record.clone());
    Ok(Some(record))
}

fn resolve_suggestion<'a>(
    records: &'a [SuggestionRecord],
    reference: &str,
) -> Option<&'a SuggestionRecord> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    if let Some(record) = records.iter().find(|record| record.id == reference) {
        return Some(record);
    }
    if let Ok(index) = reference.parse::<usize>() {
        if index > 0 {
            return records
                .iter()
                .filter(|record| record.status == SuggestionStatus::Pending)
                .nth(index - 1);
        }
    }
    records
        .iter()
        .find(|record| record.title.eq_ignore_ascii_case(reference))
}

fn set_status(
    records: &mut [SuggestionRecord],
    suggestion_id: &str,
    status: SuggestionStatus,
) -> bool {
    let Some(record) = records
        .iter_mut()
        .find(|record| record.id.as_str() == suggestion_id)
    else {
        return false;
    };
    if record.status == status {
        return false;
    }
    record.status = status;
    record.resolved_at = Some(Utc::now());
    true
}

fn suggestion_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn with_suggestion_lock<T>(
    f: impl FnOnce() -> Result<T, SuggestionError>,
) -> Result<T, SuggestionError> {
    let _guard = suggestion_lock()
        .lock()
        .map_err(|_| SuggestionError::Corrupted("suggestions lock poisoned".to_string()))?;
    f()
}

#[cfg(unix)]
fn secure_dir(path: &std::path::Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn secure_dir(_path: &std::path::Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &std::path::Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn secure_file(_path: &std::path::Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (SuggestionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SuggestionStore::with_path(dir.path().join("cron").join("suggestions.json"));
        (store, dir)
    }

    #[test]
    fn catalog_seed_lists_pending_and_is_idempotent() {
        let (store, _dir) = temp_store();
        let created = store
            .seed_catalog_suggestions()
            .expect("seed catalog suggestions");
        assert_eq!(
            created.len(),
            MAX_PENDING_SUGGESTIONS.min(blueprint_catalog().len())
        );

        let pending = store.list_pending().expect("pending");
        assert_eq!(pending.len(), created.len());
        assert!(pending.iter().any(|record| {
            record.title == "Morning briefing" && record.job_spec.schedule == "0 8 * * *"
        }));

        let created_again = store
            .seed_catalog_suggestions()
            .expect("seed catalog suggestions again");
        assert!(created_again.is_empty());
    }

    #[test]
    fn dismiss_latches_catalog_dedup() {
        let (store, _dir) = temp_store();
        store
            .seed_catalog_suggestions()
            .expect("seed catalog suggestions");

        let dismissed = store.get_pending("1").expect("pending").expect("first");
        let dismissed_key = dismissed.dedup_key.clone();
        assert!(store.dismiss_suggestion("1").expect("dismiss"));
        let pending = store.list_pending().expect("pending");
        assert_eq!(pending.len(), MAX_PENDING_SUGGESTIONS - 1);

        let _created_again = store
            .seed_catalog_suggestions()
            .expect("seed catalog suggestions again");
        let records = store.load_suggestions().expect("records");
        assert!(!records.iter().any(|record| {
            record.dedup_key == dismissed_key && record.status == SuggestionStatus::Pending
        }));
    }

    #[test]
    fn accepted_records_can_be_cleared_without_losing_dismissals() {
        let (store, _dir) = temp_store();
        let created = store
            .seed_catalog_suggestions()
            .expect("seed catalog suggestions");
        let accepted_id = created[0].id.clone();
        assert!(store.mark_accepted(&accepted_id).expect("accepted"));
        assert!(store.dismiss_suggestion("1").expect("dismiss remaining"));

        assert_eq!(store.clear_resolved().expect("clear"), 1);
        let records = store.load_suggestions().expect("records");
        assert!(records.iter().all(|record| record.id != accepted_id));
        assert!(records
            .iter()
            .any(|record| record.status == SuggestionStatus::Dismissed));
    }
}
