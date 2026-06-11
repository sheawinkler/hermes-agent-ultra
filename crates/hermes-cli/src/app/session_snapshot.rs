use std::path::{Path, PathBuf};
use std::time::SystemTime;

use uuid::Uuid;

use hermes_core::AgentError;

use super::{App, SessionInfo};
const SESSION_SNAPSHOT_MAX_FILES_DEFAULT: usize = 1500;
const SESSION_SNAPSHOT_MAX_TOTAL_BYTES_DEFAULT: u64 = 1536 * 1024 * 1024;
const SESSION_SNAPSHOT_MIN_FREE_BYTES_DEFAULT: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
struct SessionSnapshotEntry {
    path: PathBuf,
    modified: SystemTime,
    size_bytes: u64,
}

fn read_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn read_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn snapshot_max_files() -> usize {
    read_env_usize(
        "HERMES_SESSION_SNAPSHOT_MAX_FILES",
        SESSION_SNAPSHOT_MAX_FILES_DEFAULT,
    )
}

fn snapshot_max_total_bytes() -> u64 {
    read_env_u64(
        "HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES",
        SESSION_SNAPSHOT_MAX_TOTAL_BYTES_DEFAULT,
    )
}

fn snapshot_min_free_bytes() -> u64 {
    read_env_u64(
        "HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES",
        SESSION_SNAPSHOT_MIN_FREE_BYTES_DEFAULT,
    )
}

fn list_session_snapshot_entries(sessions_dir: &Path) -> Vec<SessionSnapshotEntry> {
    let mut entries: Vec<SessionSnapshotEntry> = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(sessions_dir) else {
        return entries;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        entries.push(SessionSnapshotEntry {
            path,
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size_bytes: meta.len(),
        });
    }
    entries.sort_by_key(|row| row.modified);
    entries
}

#[cfg(unix)]
fn available_disk_space_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated C string and `stats` points
    // to valid writable memory for the kernel call.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `rc == 0` means `stats` was initialized by `statvfs`.
    let stats = unsafe { stats.assume_init() };
    Some((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

#[cfg(not(unix))]
fn available_disk_space_bytes(_path: &Path) -> Option<u64> {
    None
}

impl App {
    pub(super) fn ensure_session_stub_snapshot(&self) {
        if let Err(err) = self.persist_session_snapshot(None) {
            tracing::warn!("session startup snapshot skipped: {}", err);
        }
    }
    /// Flush POI buffer, memory providers, and plugin hooks before session teardown.
    pub fn flush_session_teardown(&self, interrupted: bool) {
        let cfg = self.core.agent.config();
        if !cfg.interest.enabled && self.session.messages.is_empty() && cfg.skip_memory {
            return;
        }
        hermes_agent::hooks::session_end_hooks(
            &self.core.agent,
            &self.session.messages,
            false,
            interrupted,
            0,
            true,
        );
    }

    /// Create a new session, clearing all messages.
    pub fn new_session(&mut self) {
        self.flush_session_teardown(false);
        let old_session_id = self.session.session_id.clone();
        self.session.session_id = Uuid::new_v4().to_string();
        self.core
            .agent
            .set_runtime_session_id(&self.session.session_id);
        self.core.agent.reset_session_state(None, None, false);
        self.core.agent.reset_session_db_flush_cursor();
        self.core.agent.invalidate_cached_system_prompt();
        self.notify_memory_session_switch(
            &self.session.session_id,
            &old_session_id,
            true,
            "new_session",
        );
        self.session.messages.clear();
        self.session.ui_messages.clear();
        self.stream.pending_image_hint = None;
        self.session.session_objective = None;
        self.session.clear_input_history();
        self.ensure_session_stub_snapshot();
    }
    /// Apply the finalized messages returned by an agent run.
    pub fn apply_agent_result(&mut self, result: hermes_core::AgentResult) {
        self.session.messages = result.messages;
        self.prune_ui_after_current_messages();
    }

    /// Apply finalized messages and persist the session snapshot.
    pub fn apply_agent_result_and_persist(
        &mut self,
        result: hermes_core::AgentResult,
    ) -> Result<(), AgentError> {
        let end_of_run = result.finished_naturally || result.interrupted;
        self.apply_agent_result(result);
        self.snapshot_gate.record_mutation();
        if end_of_run || self.snapshot_gate.should_persist() {
            self.persist_session_snapshot(None).map(|_| ())?;
            self.snapshot_gate.mark_persisted();
        }
        Ok(())
    }

    /// Force a pending autosave flush (e.g. before exit).
    pub fn flush_session_snapshot(&mut self) -> Result<(), AgentError> {
        if self.snapshot_gate.pending_mutations == 0 {
            return Ok(());
        }
        self.persist_session_snapshot(None).map(|_| ())?;
        self.snapshot_gate.mark_persisted();
        Ok(())
    }

    /// Count background jobs currently queued/running.
    pub fn running_background_job_count(&self) -> usize {
        let jobs_dir = hermes_config::hermes_home().join("background_jobs");
        let mut active = 0usize;
        let entries = match std::fs::read_dir(jobs_dir) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if matches!(status, "queued" | "running") {
                active += 1;
            }
        }
        active
    }

    fn prune_session_snapshot_entry(
        entry: &SessionSnapshotEntry,
        total_bytes: &mut u64,
    ) -> Result<(), AgentError> {
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                *total_bytes = total_bytes.saturating_sub(entry.size_bytes);
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(AgentError::Io(format!(
                "Failed to prune session snapshot {}: {}",
                entry.path.display(),
                err
            ))),
        }
    }

    fn enforce_session_snapshot_guardrails(
        &self,
        sessions_dir: &Path,
        preserve_path: &Path,
    ) -> Result<(), AgentError> {
        let preserve = preserve_path.to_path_buf();
        let mut entries = list_session_snapshot_entries(sessions_dir);
        let mut total_bytes = entries.iter().map(|e| e.size_bytes).sum::<u64>();

        let max_files = snapshot_max_files();
        if max_files > 0 {
            while entries.len() > max_files {
                let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let max_total_bytes = snapshot_max_total_bytes();
        if max_total_bytes > 0 {
            while total_bytes > max_total_bytes {
                let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let min_free_bytes = snapshot_min_free_bytes();
        if min_free_bytes > 0 {
            if let Some(mut free_bytes) = available_disk_space_bytes(sessions_dir) {
                while free_bytes < min_free_bytes {
                    let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                        break;
                    };
                    let removed = entries.remove(idx);
                    Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
                    free_bytes = available_disk_space_bytes(sessions_dir).unwrap_or(free_bytes);
                }
                if free_bytes < min_free_bytes {
                    return Err(AgentError::Io(format!(
                        "Session snapshot write blocked by disk guardrail: free={} bytes, required_min={} bytes (dir={})",
                        free_bytes,
                        min_free_bytes,
                        sessions_dir.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Get a serializable snapshot of the current session info.
    pub fn session_info(&self) -> SessionInfo {
        SessionInfo {
            session_id: self.session.session_id.clone(),
            model: self.model.current_model.clone(),
            personality: self.model.current_personality.clone(),
            message_count: self.session.messages.len(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Persist a JSON session snapshot to `<state_root>/sessions`.
    ///
    /// When `name_override` is provided, that value is used as the file stem.
    /// Otherwise the active `session_id` is used.
    pub fn persist_session_snapshot(
        &self,
        name_override: Option<&str>,
    ) -> Result<PathBuf, AgentError> {
        let sessions_dir = self.state_root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create sessions dir {}: {}",
                sessions_dir.display(),
                e
            ))
        })?;
        let stem = name_override
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(self.session.session_id.as_str());
        let path = sessions_dir.join(format!("{stem}.json"));
        let payload = serde_json::json!({
            "session_info": self.session_info(),
            "messages": self.session.messages.iter().map(|m| {
                serde_json::json!({
                    "role": format!("{:?}", m.role),
                    "content": m.content.as_deref().unwrap_or(""),
                    "tool_call_id": m.tool_call_id,
                    "tool_calls": m.tool_calls,
                    "reasoning_content": m.reasoning_content,
                })
            }).collect::<Vec<_>>(),
        });
        let json = serde_json::to_string_pretty(&payload).map_err(|e| {
            AgentError::Config(format!("Failed to serialize session snapshot: {e}"))
        })?;
        std::fs::write(&path, json).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write session snapshot {}: {}",
                path.display(),
                e
            ))
        })?;
        self.enforce_session_snapshot_guardrails(&sessions_dir, &path)?;
        Ok(path)
    }
}
