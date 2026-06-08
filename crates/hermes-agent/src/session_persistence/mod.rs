//! Session persistence — SQLite-backed session storage (`hermes_state.py` parity).
//!
//! Keeps the existing `SessionPersistence` public API while aligning schema,
//! FTS, WAL fallback, and SessionDB query semantics with upstream Python.

mod maintenance;
mod queries;
mod rewind;
mod schema;
mod search;
mod telegram;
mod wal;
mod write;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use hermes_core::{AgentError, Message, MessageRole};
use rusqlite::Connection;

pub use queries::{
    AnchoredViewResult, MessagesAroundResult, SessionRecord, StoredMessageRow,
    decode_content_preview,
};
pub use rewind::{RewindOutcome, UserMessageRef};
pub use search::{SearchMessageMatch, sanitize_fts5_query};
pub use schema::SCHEMA_VERSION;
pub use wal::{format_session_db_unavailable, get_last_init_error, set_last_init_error};

// Re-export for session_search and tests.
pub use schema::init_schema;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Join leading consecutive system messages for persistence parity.
pub fn leading_system_prompt_for_persist(messages: &[Message]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        if m.role != MessageRole::System {
            break;
        }
        if let Some(c) = m
            .content
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(c.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Tracks how many transcript messages were already written for a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFlushCursor {
    pub last_flushed_db_idx: usize,
}

impl SessionFlushCursor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.last_flushed_db_idx = 0;
    }
}

/// Result of one startup auto-maintenance pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMaintenanceResult {
    pub skipped: bool,
    pub pruned: u64,
    pub vacuumed: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// SessionPersistence
// ---------------------------------------------------------------------------

/// Manages session persistence to `state.db` (Python `SessionDB` parity).
pub struct SessionPersistence {
    db_path: PathBuf,
    sessions_dir: PathBuf,
    trajectories_dir: PathBuf,
    conn: OnceLock<Arc<Mutex<Connection>>>,
    fts_enabled: OnceLock<bool>,
    write_count: AtomicU64,
}

impl SessionPersistence {
    /// Resolve the SQLite database path under `hermes_home` (see [`hermes_config::state_db_path_in`]).
    pub fn resolve_db_path(hermes_home: &Path) -> PathBuf {
        hermes_config::state_db_path_in(hermes_home)
    }

    /// Create a persistence manager rooted at the given Hermes home directory.
    pub fn new(hermes_home: impl AsRef<Path>) -> Self {
        let home = hermes_home.as_ref();
        Self {
            db_path: hermes_config::state_db_path_in(home),
            sessions_dir: home.join("sessions"),
            trajectories_dir: home.join("trajectories"),
            conn: OnceLock::new(),
            fts_enabled: OnceLock::new(),
            write_count: AtomicU64::new(0),
        }
    }

    /// Path to the SQLite database file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Whether FTS5 search indexes are available for this database.
    pub fn fts_enabled(&self) -> bool {
        *self.fts_enabled.get().unwrap_or(&true)
    }

    pub(crate) fn shared_connection(
        &self,
    ) -> Result<Arc<Mutex<Connection>>, AgentError> {
        if let Some(conn) = self.conn.get() {
            return Ok(conn.clone());
        }
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create db directory: {e}")))?;
        }
        let conn = Connection::open(&self.db_path).map_err(|e| {
            set_last_init_error(Some(format!("{e}")));
            AgentError::Io(format!("Failed to open state db: {e}"))
        })?;
        conn.busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| AgentError::Io(format!("busy_timeout: {e}")))?;
        wal::apply_wal_with_fallback(&conn, "state.db").map_err(|e| {
            let msg = format!("{e}");
            set_last_init_error(Some(msg.clone()));
            AgentError::Io(format!("WAL setup failed: {e}"))
        })?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| AgentError::Io(format!("foreign_keys: {e}")))?;
        let fts = schema::init_schema(&conn)?;
        let _ = self.fts_enabled.set(fts);
        let arc = Arc::new(Mutex::new(conn));
        match self.conn.set(arc.clone()) {
            Ok(()) => Ok(arc),
            Err(_) => Ok(self.conn.get().expect("state db conn").clone()),
        }
    }

    fn conn_arc(&self) -> Result<Arc<Mutex<Connection>>, AgentError> {
        self.ensure_db()?;
        self.shared_connection()
    }

    fn after_write(&self, conn: &Arc<Mutex<Connection>>) {
        let n = self.write_count.fetch_add(1, Ordering::Relaxed) + 1;
        if n % write::CHECKPOINT_EVERY_N_WRITES == 0 {
            write::try_wal_checkpoint(conn);
        }
    }

    /// Default home: `HERMES_HOME` → `HERMES_AGENT_ULTRA_HOME` → `~/.hermes-agent-ultra`
    /// with legacy fallback to `~/.hermes`.
    pub fn default_home() -> Self {
        if let Ok(home) = std::env::var("HERMES_HOME") {
            let home = home.trim();
            if !home.is_empty() {
                return Self::new(home);
            }
        }
        if let Ok(home) = std::env::var("HERMES_AGENT_ULTRA_HOME") {
            let home = home.trim();
            if !home.is_empty() {
                return Self::new(home);
            }
        }
        let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let primary = base.join(".hermes-agent-ultra");
        let legacy = base.join(".hermes");
        if primary.exists() || !legacy.exists() {
            Self::new(primary)
        } else {
            Self::new(legacy)
        }
    }

    pub fn ensure_db(&self) -> Result<(), AgentError> {
        let _ = self.shared_connection()?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, AgentError> {
        let conn = self.conn_arc()?;
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        match guard.query_row(
            "SELECT value FROM state_meta WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("get_meta: {e}"))),
        }
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), AgentError> {
        let conn = self.conn_arc()?;
        write::execute_write(&conn, |c| {
            c.execute(
                "INSERT INTO state_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )
            .map_err(|e| AgentError::Io(format!("set_meta: {e}")))?;
            Ok(())
        })?;
        self.after_write(&conn);
        Ok(())
    }

    pub fn fts_index_count(&self) -> Result<u32, AgentError> {
        let conn = self.conn_arc()?;
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        maintenance::fts_index_count(&guard)
    }

    pub fn optimize_fts(&self) -> Result<u32, AgentError> {
        let conn = self.conn_arc()?;
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        maintenance::optimize_fts_on_conn(&guard)
    }

    pub fn truncate_wal_checkpoint(&self) -> Result<(), AgentError> {
        maintenance::truncate_wal_checkpoint(&self.conn_arc()?)
    }

    pub fn vacuum(&self) -> Result<(), AgentError> {
        let conn = self.conn_arc()?;
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        if let Err(err) = maintenance::optimize_fts_on_conn(&guard) {
            tracing::warn!("FTS optimize before VACUUM failed: {}", err);
        }
        guard
            .execute_batch("VACUUM")
            .map_err(|e| AgentError::Io(format!("VACUUM failed: {e}")))?;
        Ok(())
    }

    /// Delete ended sessions older than `older_than_days` (Python `prune_sessions`).
    pub fn prune_sessions(&self, older_than_days: u32) -> Result<u64, AgentError> {
        let cutoff = queries::now_unix() - (older_than_days as f64 * 86400.0);
        let conn = self.conn_arc()?;
        let removed = write::execute_write(&conn, |c| {
            let ids: Vec<String> = c
                .prepare(
                    "SELECT id FROM sessions WHERE started_at < ?1 AND ended_at IS NOT NULL",
                )
                .map_err(|e| AgentError::Io(format!("prune prepare: {e}")))?
                .query_map(rusqlite::params![cutoff], |r| r.get(0))
                .map_err(|e| AgentError::Io(format!("prune query: {e}")))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| AgentError::Io(format!("prune read: {e}")))?;
            if ids.is_empty() {
                return Ok(0u64);
            }
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            c.execute(
                &format!(
                    "UPDATE sessions SET parent_session_id = NULL
                     WHERE parent_session_id IN ({placeholders})"
                ),
                rusqlite::params_from_iter(ids.iter()),
            )
            .map_err(|e| AgentError::Io(format!("prune orphan: {e}")))?;
            for sid in &ids {
                c.execute(
                    "DELETE FROM messages WHERE session_id = ?1",
                    rusqlite::params![sid],
                )
                .map_err(|e| AgentError::Io(format!("prune messages: {e}")))?;
                c.execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![sid])
                    .map_err(|e| AgentError::Io(format!("prune session: {e}")))?;
            }
            Ok(ids.len() as u64)
        })?;
        self.after_write(&conn);
        Ok(removed)
    }

    pub fn maybe_auto_prune_and_vacuum(
        &self,
        retention_days: u32,
        min_interval_hours: u32,
        vacuum_after_prune: bool,
    ) -> AutoMaintenanceResult {
        let mut result = AutoMaintenanceResult {
            skipped: false,
            pruned: 0,
            vacuumed: false,
            error: None,
        };
        let now = queries::now_unix();
        let min_seconds = (min_interval_hours as f64) * 3600.0;
        match self.get_meta("last_auto_prune") {
            Ok(Some(last_raw)) => {
                if let Ok(last_ts) = last_raw.parse::<f64>() {
                    if now - last_ts < min_seconds {
                        result.skipped = true;
                        return result;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                result.error = Some(e.to_string());
                return result;
            }
        }
        match self.prune_sessions(retention_days) {
            Ok(pruned) => {
                result.pruned = pruned;
                if vacuum_after_prune && pruned > 0 {
                    match self.vacuum() {
                        Ok(()) => result.vacuumed = true,
                        Err(e) => tracing::warn!("state.db VACUUM failed: {}", e),
                    }
                }
            }
            Err(e) => {
                result.error = Some(e.to_string());
                return result;
            }
        }
        if let Err(e) = self.set_meta("last_auto_prune", &now.to_string()) {
            result.error = Some(e.to_string());
            return result;
        }
        if let Err(e) = self.truncate_wal_checkpoint() {
            tracing::warn!("state.db WAL checkpoint failed: {}", e);
        }
        result
    }

    pub fn update_session_model(&self, session_id: &str, model: &str) -> Result<bool, AgentError> {
        queries::update_session_model(&self.conn_arc()?, session_id, model)
    }

    pub fn get_session_model(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        queries::get_session_model(&self.conn_arc()?, session_id)
    }

    pub fn rewind_active_user_turns(
        &self,
        session_id: &str,
        user_turns_back: usize,
    ) -> Result<Option<RewindOutcome>, AgentError> {
        rewind::rewind_active_user_turns(&self.conn_arc()?, session_id, user_turns_back)
    }

    pub fn list_recent_user_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<UserMessageRef>, AgentError> {
        rewind::list_recent_user_messages(&self.conn_arc()?, session_id, limit)
    }

    pub fn restore_rewound_since(
        &self,
        session_id: &str,
        since_message_id: i64,
    ) -> Result<u64, AgentError> {
        rewind::restore_rewound_since(&self.conn_arc()?, session_id, since_message_id)
    }

    pub fn persist_session(
        &self,
        session_id: &str,
        messages: &[Message],
        cursor: &mut SessionFlushCursor,
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<(), AgentError> {
        self.persist_session_with_history_len(
            session_id,
            messages,
            cursor,
            None,
            model,
            platform,
            title,
            system_prompt,
        )
    }

    pub fn persist_session_with_history_len(
        &self,
        session_id: &str,
        messages: &[Message],
        cursor: &mut SessionFlushCursor,
        conversation_history_len: Option<usize>,
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<(), AgentError> {
        let source = platform.unwrap_or("cli");
        queries::ensure_session(&self.conn_arc()?, session_id, source, model)?;

        let conn = self.conn_arc()?;
        let sid = session_id.to_string();
        let model = model.map(str::to_string);
        let title = title.map(str::to_string);
        let sp = system_prompt.map(str::to_string);
        let count = messages.len() as i64;

        write::execute_write(&conn, move |c| {
            c.execute(
                "UPDATE sessions SET
                    message_count = ?1,
                    model = COALESCE(?2, model),
                    title = COALESCE(?3, title),
                    system_prompt = COALESCE(?4, system_prompt),
                    source = COALESCE(source, 'cli')
                 WHERE id = ?5",
                rusqlite::params![count, model, title, sp, sid],
            )
            .map_err(|e| AgentError::Io(format!("persist session row: {e}")))?;
            Ok(())
        })?;

        let history_start = conversation_history_len.unwrap_or(0);
        let flush_from = history_start.max(cursor.last_flushed_db_idx);
        if flush_from < messages.len() {
            queries::append_messages(&conn, session_id, &messages[flush_from..])?;
            self.after_write(&conn);
        }
        cursor.last_flushed_db_idx = messages.len();
        Ok(())
    }

    pub fn replace_session_messages(
        &self,
        session_id: &str,
        messages: &[Message],
        cursor: &mut SessionFlushCursor,
    ) -> Result<(), AgentError> {
        let conn = self.conn_arc()?;
        let sid = session_id.to_string();
        write::execute_write(&conn, move |c| {
            let sql = if schema::table_has_column_pub(c, "messages", "active") {
                "DELETE FROM messages WHERE session_id = ?1 AND active = 1"
            } else {
                "DELETE FROM messages WHERE session_id = ?1"
            };
            c.execute(sql, rusqlite::params![sid])
                .map_err(|e| AgentError::Io(format!("replace clear: {e}")))?;
            Ok(())
        })?;
        queries::append_messages(&conn, session_id, messages)?;
        let count = messages.len() as i64;
        write::execute_write(&conn, move |c| {
            c.execute(
                "UPDATE sessions SET message_count = ?1 WHERE id = ?2",
                rusqlite::params![count, session_id],
            )
            .map_err(|e| AgentError::Io(format!("replace count: {e}")))?;
            Ok(())
        })?;
        cursor.last_flushed_db_idx = messages.len();
        self.after_write(&conn);
        Ok(())
    }

    pub fn save_session_log(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
    ) -> Result<PathBuf, AgentError> {
        std::fs::create_dir_all(&self.sessions_dir)
            .map_err(|e| AgentError::Io(format!("sessions dir: {e}")))?;
        let timestamp = Utc::now().format("%Y-%m-%d_%H%M%S");
        let path = self
            .sessions_dir
            .join(format!("{timestamp}-{session_id}.md"));
        let mut content = format!("# Session: {session_id}\n\n");
        if let Some(m) = model {
            content.push_str(&format!("Model: {m}\n"));
        }
        content.push_str(&format!(
            "Date: {}\n\n---\n\n",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ));
        for msg in messages {
            let label = match msg.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::Tool => "Tool",
            };
            content.push_str(&format!("### {label}\n\n"));
            if let Some(ref text) = msg.content {
                content.push_str(text);
                content.push('\n');
            }
        }
        std::fs::write(&path, content)
            .map_err(|e| AgentError::Io(format!("write session log: {e}")))?;
        Ok(path)
    }

    pub fn save_trajectory(
        &self,
        session_id: &str,
        messages: &[Message],
        user_query: &str,
        completed: bool,
    ) -> Result<PathBuf, AgentError> {
        std::fs::create_dir_all(&self.trajectories_dir)
            .map_err(|e| AgentError::Io(format!("trajectories dir: {e}")))?;
        let path = self.trajectories_dir.join(format!(
            "{}-{}.json",
            Utc::now().format("%Y-%m-%d_%H%M%S"),
            session_id
        ));
        let trajectory = serde_json::json!({
            "session_id": session_id,
            "user_query": user_query,
            "completed": completed,
            "timestamp": Utc::now().to_rfc3339(),
            "messages": messages,
        });
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&trajectory)
                .map_err(|e| AgentError::Io(format!("serialize trajectory: {e}")))?,
        )
        .map_err(|e| AgentError::Io(format!("write trajectory: {e}")))?;
        Ok(path)
    }

    pub fn get_indexed_session_id(&self, session_key: &str) -> Result<Option<String>, AgentError> {
        let conn = self.conn_arc()?;
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        match guard.query_row(
            "SELECT session_id FROM gateway_session_index WHERE session_key = ?1",
            rusqlite::params![session_key],
            |r| r.get(0),
        ) {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("get_indexed_session_id: {e}"))),
        }
    }

    pub fn upsert_session_index(&self, session_key: &str, session_id: &str) -> Result<(), AgentError> {
        let conn = self.conn_arc()?;
        write::execute_write(&conn, |c| {
            c.execute(
                "INSERT INTO gateway_session_index (session_key, session_id)
                 VALUES (?1, ?2)
                 ON CONFLICT(session_key) DO UPDATE SET session_id = ?2",
                rusqlite::params![session_key, session_id],
            )
            .map_err(|e| AgentError::Io(format!("upsert_session_index: {e}")))?;
            Ok(())
        })?;
        self.after_write(&conn);
        Ok(())
    }

    pub fn update_system_prompt(&self, session_id: &str, system_prompt: &str) -> Result<(), AgentError> {
        let conn = self.conn_arc()?;
        let sid = session_id.to_string();
        let sp = system_prompt.to_string();
        write::execute_write(&conn, move |c| {
            c.execute(
                "UPDATE sessions SET system_prompt = ?1 WHERE id = ?2",
                rusqlite::params![sp, sid],
            )
            .map_err(|e| AgentError::Io(format!("update_system_prompt: {e}")))?;
            Ok(())
        })?;
        self.after_write(&conn);
        Ok(())
    }

    pub fn create_compression_continuation_session(
        &self,
        new_session_id: &str,
        parent_session_id: &str,
        model: Option<&str>,
        platform: Option<&str>,
        system_prompt: &str,
    ) -> Result<(), AgentError> {
        let source = platform.unwrap_or("cli");
        queries::create_session(
            &self.conn_arc()?,
            new_session_id,
            source,
            model,
            Some(parent_session_id),
            Some(system_prompt),
            None,
        )?;
        queries::end_session(&self.conn_arc()?, parent_session_id, "compression")?;
        Ok(())
    }

    pub fn try_acquire_compression_lock(
        &self,
        session_id: &str,
        holder: &str,
        ttl_seconds: f64,
    ) -> Result<bool, AgentError> {
        if session_id.is_empty() {
            return Ok(false);
        }
        let conn = self.conn_arc()?;
        let now = queries::now_unix();
        let expires = now + ttl_seconds;
        let sid = session_id.to_string();
        let holder = holder.to_string();
        let acquired = write::execute_write(&conn, move |c| {
            c.execute(
                "DELETE FROM compression_locks WHERE session_id = ?1 AND expires_at < ?2",
                rusqlite::params![sid, now],
            )
            .map_err(|e| AgentError::Io(format!("lock reclaim: {e}")))?;
            c.execute(
                "INSERT OR IGNORE INTO compression_locks (session_id, holder, acquired_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![sid, holder, now, expires],
            )
            .map_err(|e| AgentError::Io(format!("lock insert: {e}")))?;
            let owner: Option<String> = c
                .query_row(
                    "SELECT holder FROM compression_locks WHERE session_id = ?1",
                    rusqlite::params![sid],
                    |r| r.get(0),
                )
                .ok();
            Ok(owner.as_deref() == Some(holder.as_str()))
        });
        match acquired {
            Ok(v) => {
                self.after_write(&conn);
                Ok(v)
            }
            Err(_) => Ok(false),
        }
    }

    pub fn release_compression_lock(&self, session_id: &str, holder: &str) -> Result<(), AgentError> {
        if session_id.is_empty() {
            return Ok(());
        }
        let conn = self.conn_arc()?;
        let sid = session_id.to_string();
        let holder = holder.to_string();
        write::execute_write(&conn, move |c| {
            c.execute(
                "DELETE FROM compression_locks WHERE session_id = ?1 AND holder = ?2",
                rusqlite::params![sid, holder],
            )
            .map_err(|e| AgentError::Io(format!("release lock: {e}")))?;
            Ok(())
        })?;
        self.after_write(&conn);
        Ok(())
    }

    pub fn get_compression_lock_holder(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        if session_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn_arc()?;
        let now = queries::now_unix();
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        match guard.query_row(
            "SELECT holder FROM compression_locks WHERE session_id = ?1 AND expires_at >= ?2",
            rusqlite::params![session_id, now],
            |r| r.get(0),
        ) {
            Ok(h) => Ok(Some(h)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("get_compression_lock_holder: {e}"))),
        }
    }

    pub fn get_system_prompt(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        Ok(queries::get_session(&self.conn_arc()?, session_id)?
            .and_then(|s| s.system_prompt)
            .filter(|t| !t.trim().is_empty()))
    }

    pub fn load_session(&self, session_id: &str) -> Result<Vec<Message>, AgentError> {
        let resolved = queries::resolve_resume_session_id(&self.conn_arc()?, session_id)?;
        queries::load_messages(&self.conn_arc()?, &resolved)
    }

    // ── SessionDB parity surface ─────────────────────────────────────────

    pub fn create_session(
        &self,
        session_id: &str,
        source: &str,
        model: Option<&str>,
    ) -> Result<(), AgentError> {
        queries::create_session(
            &self.conn_arc()?,
            session_id,
            source,
            model,
            None,
            None,
            None,
        )
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>, AgentError> {
        queries::get_session(&self.conn_arc()?, session_id)
    }

    pub fn set_session_title(&self, session_id: &str, title: Option<&str>) -> Result<bool, AgentError> {
        queries::set_session_title(&self.conn_arc()?, session_id, title)
    }

    pub fn get_session_title(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        queries::get_session_title(&self.conn_arc()?, session_id)
    }

    pub fn resolve_session_id(&self, session_id_or_prefix: &str) -> Result<Option<String>, AgentError> {
        queries::resolve_session_id(&self.conn_arc()?, session_id_or_prefix)
    }

    pub fn resolve_session_by_title(&self, title: &str) -> Result<Option<String>, AgentError> {
        queries::resolve_session_by_title(&self.conn_arc()?, title)
    }

    pub fn resolve_resume_session_id(&self, session_id: &str) -> Result<String, AgentError> {
        queries::resolve_resume_session_id(&self.conn_arc()?, session_id)
    }

    pub fn list_sessions_rich(
        &self,
        source: Option<&str>,
        exclude_sources: &[&str],
        limit: usize,
        offset: usize,
        min_message_count: i64,
        order_by_last_active: bool,
    ) -> Result<Vec<SessionRecord>, AgentError> {
        self.list_sessions_rich_full(
            source,
            exclude_sources,
            limit,
            offset,
            min_message_count,
            false,
            true,
            order_by_last_active,
        )
    }

    pub fn list_sessions_rich_full(
        &self,
        source: Option<&str>,
        exclude_sources: &[&str],
        limit: usize,
        offset: usize,
        min_message_count: i64,
        include_children: bool,
        project_compression_tips: bool,
        order_by_last_active: bool,
    ) -> Result<Vec<SessionRecord>, AgentError> {
        queries::list_sessions_rich(
            &self.conn_arc()?,
            source,
            exclude_sources,
            limit,
            offset,
            min_message_count,
            include_children,
            project_compression_tips,
            order_by_last_active,
        )
    }

    pub fn get_compression_tip(&self, session_id: &str) -> Result<String, AgentError> {
        queries::get_compression_tip(&self.conn_arc()?, session_id)
    }

    pub fn search_messages(
        &self,
        query: &str,
        source_filter: Option<&[&str]>,
        exclude_sources: Option<&[&str]>,
        role_filter: Option<&[&str]>,
        limit: usize,
        offset: usize,
        sort: Option<&str>,
    ) -> Result<Vec<SearchMessageMatch>, AgentError> {
        search::search_messages(
            &self.conn_arc()?,
            query,
            source_filter,
            exclude_sources,
            role_filter,
            limit,
            offset,
            sort,
        )
    }

    pub fn get_messages_around(
        &self,
        session_id: &str,
        around_message_id: i64,
        window: i64,
    ) -> Result<MessagesAroundResult, AgentError> {
        queries::get_messages_around(
            &self.conn_arc()?,
            session_id,
            around_message_id,
            window,
        )
    }

    pub fn get_anchored_view(
        &self,
        session_id: &str,
        around_message_id: i64,
        window: i64,
        bookend: i64,
        keep_roles: Option<&[&str]>,
    ) -> Result<AnchoredViewResult, AgentError> {
        queries::get_anchored_view(
            &self.conn_arc()?,
            session_id,
            around_message_id,
            window,
            bookend,
            keep_roles,
        )
    }

    pub fn apply_telegram_topic_migration(&self) -> Result<(), AgentError> {
        telegram::apply_telegram_topic_migration(&self.conn_arc()?)
    }

    pub fn end_session(&self, session_id: &str, end_reason: &str) -> Result<(), AgentError> {
        queries::end_session(&self.conn_arc()?, session_id, end_reason)
    }

    pub fn reopen_session(&self, session_id: &str) -> Result<(), AgentError> {
        queries::reopen_session(&self.conn_arc()?, session_id)
    }

    pub fn update_token_counts(
        &self,
        session_id: &str,
        update: &hermes_tools::state_db::TokenCountUpdate,
    ) -> Result<(), AgentError> {
        queries::update_token_counts(&self.conn_arc()?, session_id, update)
    }

    pub fn enable_telegram_topic_mode(
        &self,
        chat_id: &str,
        user_id: &str,
        has_topics_enabled: Option<bool>,
        allows_users_to_create_topics: Option<bool>,
    ) -> Result<(), AgentError> {
        hermes_tools::state_db::enable_telegram_topic_mode(
            &self.conn_arc()?,
            chat_id,
            user_id,
            has_topics_enabled,
            allows_users_to_create_topics,
        )
        .map_err(|e| AgentError::Io(e.to_string()))
    }

    pub fn disable_telegram_topic_mode(
        &self,
        chat_id: &str,
        clear_bindings: bool,
    ) -> Result<(), AgentError> {
        hermes_tools::state_db::disable_telegram_topic_mode(
            &self.conn_arc()?,
            chat_id,
            clear_bindings,
        )
        .map_err(|e| AgentError::Io(e.to_string()))
    }

    pub fn is_telegram_topic_mode_enabled(&self, chat_id: &str, user_id: &str) -> bool {
        self.conn_arc()
            .ok()
            .is_some_and(|c| {
                hermes_tools::state_db::is_telegram_topic_mode_enabled(&c, chat_id, user_id)
            })
    }

    pub fn get_telegram_topic_binding(
        &self,
        chat_id: &str,
        thread_id: &str,
    ) -> Result<Option<telegram::TelegramTopicBinding>, AgentError> {
        hermes_tools::state_db::get_telegram_topic_binding(&self.conn_arc()?, chat_id, thread_id)
            .map_err(|e| AgentError::Io(e.to_string()))
    }

    pub fn bind_telegram_topic(
        &self,
        chat_id: &str,
        thread_id: &str,
        user_id: &str,
        session_key: &str,
        session_id: &str,
        managed_mode: &str,
    ) -> Result<(), AgentError> {
        hermes_tools::state_db::bind_telegram_topic(
            &self.conn_arc()?,
            chat_id,
            thread_id,
            user_id,
            session_key,
            session_id,
            managed_mode,
        )
        .map_err(|e| AgentError::Io(e.to_string()))
    }

    pub fn list_unlinked_telegram_sessions_for_user(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<telegram::UnlinkedTelegramSession>, AgentError> {
        hermes_tools::state_db::list_unlinked_telegram_sessions_for_user(
            &self.conn_arc()?,
            user_id,
            limit,
        )
        .map_err(|e| AgentError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn mark_session_old(sp: &SessionPersistence, session_id: &str, days_old: i64) {
        let conn = Connection::open(&sp.db_path).expect("open db");
        let ts = queries::now_unix() - (days_old as f64 * 86400.0);
        conn.execute(
            "UPDATE sessions SET started_at = ?1 WHERE id = ?2",
            params![ts, session_id],
        )
        .expect("mark old");
    }

    fn mark_session_ended_old(sp: &SessionPersistence, session_id: &str, days_old: i64) {
        let conn = Connection::open(&sp.db_path).expect("open db");
        let ts = queries::now_unix() - (days_old as f64 * 86400.0);
        conn.execute(
            "UPDATE sessions SET started_at = ?1, ended_at = ?1 WHERE id = ?2",
            params![ts, session_id],
        )
        .expect("mark ended old");
    }

    #[test]
    fn test_persist_and_load_session() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let mut assistant = Message::assistant("Hi there!");
        assistant.reasoning_content = Some("provider scratchpad".to_string());
        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            assistant,
        ];
        let mut cursor = SessionFlushCursor::new();
        sp.persist_session(
            "test-session-1",
            &messages,
            &mut cursor,
            Some("gpt-4o"),
            None,
            Some("Test"),
            Some("cached system blob"),
        )
        .unwrap();
        assert_eq!(
            sp.get_system_prompt("test-session-1").unwrap().as_deref(),
            Some("cached system blob")
        );
        let loaded = sp.load_session("test-session-1").unwrap();
        assert_eq!(loaded.len(), 3);
    }

    #[test]
    fn test_fts_after_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let mut cursor = SessionFlushCursor::new();
        sp.persist_session(
            "fts-sync",
            &[Message::user("alpha"), Message::user("beta")],
            &mut cursor,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let conn = Connection::open(sp.db_path()).unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'beta'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn test_prune_ended_sessions_only() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session(
            "ended",
            &[Message::user("x")],
            &mut SessionFlushCursor::new(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        sp.end_session("ended", "normal").unwrap();
        mark_session_ended_old(&sp, "ended", 100);
        sp.persist_session(
            "active",
            &[Message::user("y")],
            &mut SessionFlushCursor::new(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        mark_session_old(&sp, "active", 100);
        assert_eq!(sp.prune_sessions(90).unwrap(), 1);
        assert!(sp.load_session("active").unwrap().len() == 1);
    }

    #[test]
    fn resolve_db_path_prefers_state_db() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("state.db"), b"").unwrap();
        std::fs::write(tmp.path().join("sessions.db"), b"").unwrap();
        assert!(SessionPersistence::resolve_db_path(tmp.path()).ends_with("state.db"));
    }

    #[test]
    fn shared_connection_is_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let c1 = sp.shared_connection().unwrap();
        let c2 = sp.shared_connection().unwrap();
        assert!(Arc::ptr_eq(&c1, &c2));
    }

    #[test]
    fn update_session_model_only_updates_existing_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        assert!(!sp
            .update_session_model("missing-session", "openai:gpt-4o")
            .unwrap());
        let mut cursor = SessionFlushCursor::new();
        sp.persist_session(
            "existing-session",
            &[Message::user("hello")],
            &mut cursor,
            Some("openai:gpt-4o"),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(sp
            .update_session_model("existing-session", "anthropic:claude-sonnet")
            .unwrap());
        assert_eq!(
            sp.get_session_model("existing-session").unwrap().as_deref(),
            Some("anthropic:claude-sonnet")
        );
    }

    #[test]
    fn rewind_soft_deletes_user_turns_and_loads_only_active_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![
            Message::system("sys"),
            Message::user("question 1"),
            Message::assistant("answer 1"),
            Message::user("question 2"),
            Message::assistant("answer 2"),
            Message::user("question 3"),
            Message::assistant("answer 3"),
        ];
        let mut cursor = SessionFlushCursor::new();
        sp.replace_session_messages("rewind-n", &messages, &mut cursor)
            .unwrap();
        let recent = sp.list_recent_user_messages("rewind-n", 2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].content.as_deref(), Some("question 3"));
        assert_eq!(recent[1].content.as_deref(), Some("question 2"));

        let outcome = sp.rewind_active_user_turns("rewind-n", 2).unwrap().unwrap();
        assert_eq!(outcome.target_content.as_deref(), Some("question 2"));
        assert_eq!(outcome.inactive_count, 4);
        assert_eq!(outcome.active_message_count, 3);
        assert_eq!(outcome.rewind_count, 1);

        let loaded = sp.load_session("rewind-n").unwrap();
        assert_eq!(
            loaded
                .iter()
                .filter_map(|m| m.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["sys", "question 1", "answer 1"]
        );

        let conn = Connection::open(sp.db_path()).unwrap();
        let inactive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id='rewind-n' AND active=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(inactive, 4);

        assert_eq!(
            sp.restore_rewound_since("rewind-n", outcome.target_message_id)
                .unwrap(),
            4
        );
        assert_eq!(sp.load_session("rewind-n").unwrap().len(), messages.len());
    }

    #[test]
    fn replace_session_messages_preserves_inactive_rewind_audit_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let mut cursor = SessionFlushCursor::new();
        sp.replace_session_messages(
            "audit-preserve",
            &[
                Message::user("rewind me"),
                Message::assistant("gone"),
            ],
            &mut cursor,
        )
        .unwrap();
        sp.rewind_active_user_turns("audit-preserve", 1)
            .unwrap()
            .unwrap();
        sp.replace_session_messages(
            "audit-preserve",
            &[Message::user("keep"), Message::assistant("replacement")],
            &mut cursor,
        )
        .unwrap();

        let conn = Connection::open(sp.db_path()).unwrap();
        let inactive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id='audit-preserve' AND active=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(inactive, 2);
        let loaded = sp.load_session("audit-preserve").unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn optimize_fts_returns_existing_index_count() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session(
            "opt-count",
            &[Message::user("hello optimized world")],
            &mut SessionFlushCursor::new(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let conn = Connection::open(sp.db_path()).unwrap();
        let expected = sp.fts_index_count().unwrap();
        assert_eq!(sp.optimize_fts().unwrap(), expected);
        assert!(expected >= 1);
        let _ = conn;
    }
}
