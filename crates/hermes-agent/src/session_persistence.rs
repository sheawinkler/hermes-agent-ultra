//! Session persistence — save and load conversation sessions.
//!
//! Provides SQLite-backed session storage with optional FTS5 indexing for search,
//! human-readable markdown session logs, and trajectory format for RL training.
//!
//! Corresponds to Python `run_agent.py`'s `_persist_session`, `_save_session_log`,
//! and `_save_trajectory` methods.

use std::path::{Path, PathBuf};

use chrono::{Duration as ChronoDuration, Utc};
use hermes_core::{AgentError, Message, MessageRole};

// ---------------------------------------------------------------------------
// SessionPersistence
// ---------------------------------------------------------------------------

/// Join leading consecutive system messages (Python `_cached_system_prompt` / Anthropic prefix parity for persistence).
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

/// Manages session persistence to SQLite and markdown log files.
pub struct SessionPersistence {
    /// Path to the SQLite database file.
    db_path: PathBuf,
    /// Directory for session log files.
    sessions_dir: PathBuf,
    /// Directory for trajectory files.
    trajectories_dir: PathBuf,
}

/// Result of one startup auto-maintenance pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMaintenanceResult {
    pub skipped: bool,
    pub pruned: u64,
    pub vacuumed: bool,
    pub error: Option<String>,
}

/// Result of a soft rewind operation against persisted session rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindOutcome {
    pub target_message_id: i64,
    pub target_content: Option<String>,
    pub inactive_count: u64,
    pub active_message_count: u64,
    pub rewind_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessageRef {
    pub id: i64,
    pub content: Option<String>,
}

impl SessionPersistence {
    fn is_fts5_unavailable_message(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("no such module")
            || lower.contains("unknown tokenizer")
            || lower.contains("fts5")
    }

    fn is_fts5_unavailable_error(error: &rusqlite::Error) -> bool {
        Self::is_fts5_unavailable_message(&error.to_string())
    }

    fn fts_table_exists(conn: &rusqlite::Connection) -> Result<bool, AgentError> {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages_fts' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            other => Err(AgentError::Io(format!(
                "Failed to inspect messages_fts availability: {other}"
            ))),
        })
    }

    fn ensure_fts_schema(conn: &rusqlite::Connection, db_path: &Path) -> Result<bool, AgentError> {
        match conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                session_id UNINDEXED,
                role UNINDEXED,
                content='messages',
                content_rowid='id'
            );

            CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content, session_id, role)
                VALUES (new.id, new.content, new.session_id, new.role);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                DELETE FROM messages_fts WHERE rowid = old.id;
            END;

            CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
                DELETE FROM messages_fts WHERE rowid = old.id;
                INSERT INTO messages_fts(rowid, content, session_id, role)
                VALUES (new.id, new.content, new.session_id, new.role);
            END;",
        ) {
            Ok(()) => Ok(true),
            Err(err) if Self::is_fts5_unavailable_error(&err) => {
                tracing::warn!(
                    "SQLite FTS5 unavailable for {}; session persistence will continue without full-text indexing: {}",
                    db_path.display(),
                    err
                );
                Ok(false)
            }
            Err(err) => Err(AgentError::Io(format!(
                "Failed to create FTS schema for sessions db: {err}"
            ))),
        }
    }

    fn delete_fts_rows_for_session(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> Result<(), AgentError> {
        if !Self::fts_table_exists(conn)? {
            return Ok(());
        }
        conn.execute(
            "DELETE FROM messages_fts WHERE session_id = ?1",
            rusqlite::params![session_id],
        )
        .map(|_| ())
        .map_err(|e| AgentError::Io(format!("Failed to delete messages_fts rows: {e}")))
    }

    fn ensure_text_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
    ) -> Result<(), AgentError> {
        match conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"), []) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column") {
                    Ok(())
                } else {
                    Err(AgentError::Io(format!(
                        "Failed to migrate {table}.{column}: {e}"
                    )))
                }
            }
        }
    }

    fn ensure_integer_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
        default_value: i64,
        not_null: bool,
    ) -> Result<(), AgentError> {
        let mut sql = format!("ALTER TABLE {table} ADD COLUMN {column} INTEGER");
        if not_null {
            sql.push_str(" NOT NULL");
        }
        sql.push_str(&format!(" DEFAULT {default_value}"));
        match conn.execute(&sql, []) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column") {
                    Ok(())
                } else {
                    Err(AgentError::Io(format!(
                        "Failed to migrate {table}.{column}: {e}"
                    )))
                }
            }
        }
    }

    /// Create a new persistence manager rooted at the given hermes home directory.
    pub fn new(hermes_home: impl AsRef<Path>) -> Self {
        let home = hermes_home.as_ref();
        Self {
            db_path: home.join("sessions.db"),
            sessions_dir: home.join("sessions"),
            trajectories_dir: home.join("trajectories"),
        }
    }

    /// Create using default home resolution:
    /// `HERMES_HOME` → `HERMES_AGENT_ULTRA_HOME` → `~/.hermes-agent-ultra`
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

    /// Ensure the SQLite database and tables exist.
    pub fn ensure_db(&self) -> Result<(), AgentError> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create db directory: {e}")))?;
        }

        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                title TEXT,
                message_count INTEGER DEFAULT 0,
                system_prompt TEXT,
                parent_session_id TEXT,
                model_config TEXT,
                end_reason TEXT,
                ended_at TEXT,
                rewind_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                reasoning_content TEXT,
                created_at TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id);

            CREATE TABLE IF NOT EXISTS state_meta (
                key TEXT PRIMARY KEY,
                value TEXT
            );",
        )
        .map_err(|e| AgentError::Io(format!("Failed to create tables: {e}")))?;

        for column in [
            "system_prompt",
            "parent_session_id",
            "model_config",
            "end_reason",
            "ended_at",
        ] {
            Self::ensure_text_column(&conn, "sessions", column)?;
        }
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN reasoning_content TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.reasoning_content: {e}"
                )));
            }
        }
        Self::ensure_integer_column(&conn, "sessions", "rewind_count", 0, true)?;
        Self::ensure_integer_column(&conn, "messages", "active", 1, true)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_session_active
                ON messages(session_id, active, id)",
            [],
        )
        .map_err(|e| AgentError::Io(format!("Failed to create active message index: {e}")))?;
        Self::ensure_fts_schema(&conn, &self.db_path)?;

        Ok(())
    }

    /// Read a metadata key from `state_meta`.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>, AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT value FROM state_meta WHERE key = ?1")
            .map_err(|e| AgentError::Io(format!("Failed to prepare meta query: {e}")))?;
        match stmt.query_row(rusqlite::params![key], |row| row.get::<_, String>(0)) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("Failed to read state_meta: {e}"))),
        }
    }

    /// Upsert a metadata key in `state_meta`.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        conn.execute(
            "INSERT INTO state_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(|e| AgentError::Io(format!("Failed to upsert state_meta: {e}")))?;
        Ok(())
    }

    /// Reclaim free pages in `sessions.db` after large prune operations.
    pub fn vacuum(&self) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        conn.execute_batch("VACUUM")
            .map_err(|e| AgentError::Io(format!("Failed to VACUUM sessions db: {e}")))?;
        Ok(())
    }

    /// Flush committed WAL frames and truncate the `sessions.db-wal` sidecar.
    ///
    /// SQLite's PASSIVE checkpoint leaves the WAL file at its high-water mark;
    /// TRUNCATE reclaims the sidecar after startup maintenance or explicit
    /// shutdown hooks. Databases not currently in WAL mode accept this pragma
    /// and return a no-op result.
    pub fn truncate_wal_checkpoint(&self) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| AgentError::Io(format!("Failed to checkpoint sessions WAL: {e}")))?;

        if busy > 0 {
            tracing::warn!(
                "sessions.db WAL checkpoint could not truncate immediately: busy={}, log_frames={}, checkpointed_frames={}",
                busy,
                log_frames,
                checkpointed_frames
            );
        } else if log_frames > 0 {
            tracing::debug!(
                "sessions.db WAL checkpoint truncated {} frame(s); checkpointed={}",
                log_frames,
                checkpointed_frames
            );
        }
        Ok(())
    }

    /// Delete sessions whose `updated_at` is older than the retention window.
    ///
    /// Returns the number of deleted sessions.
    pub fn prune_sessions(&self, older_than_days: u32) -> Result<u64, AgentError> {
        self.ensure_db()?;
        let cutoff = (Utc::now() - ChronoDuration::days(older_than_days as i64)).to_rfc3339();
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        let mut session_ids: Vec<String> = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT id FROM sessions WHERE updated_at < ?1")
                .map_err(|e| AgentError::Io(format!("Failed to prepare prune query: {e}")))?;
            let rows = stmt
                .query_map(rusqlite::params![cutoff], |row| row.get::<_, String>(0))
                .map_err(|e| AgentError::Io(format!("Failed to query stale sessions: {e}")))?;
            for row in rows {
                session_ids.push(
                    row.map_err(|e| AgentError::Io(format!("Failed to read session id: {e}")))?,
                );
            }
        }
        if session_ids.is_empty() {
            return Ok(0);
        }

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| AgentError::Io(format!("Failed to open prune transaction: {e}")))?;
        for sid in &session_ids {
            Self::delete_fts_rows_for_session(&tx, sid)?;
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                rusqlite::params![sid],
            )
            .map_err(|e| AgentError::Io(format!("Failed to delete message rows: {e}")))?;
            tx.execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![sid])
                .map_err(|e| AgentError::Io(format!("Failed to delete session row: {e}")))?;
        }
        tx.commit()
            .map_err(|e| AgentError::Io(format!("Failed to commit prune transaction: {e}")))?;
        Ok(session_ids.len() as u64)
    }

    /// Opportunistic startup maintenance with interval gating via `state_meta`.
    ///
    /// Never propagates errors to callers.
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

        let now = Utc::now().timestamp() as f64;
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
                        Err(e) => tracing::warn!("sessions.db VACUUM failed: {}", e),
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
            tracing::warn!("sessions.db WAL checkpoint failed: {}", e);
        }

        result
    }

    /// Persist a session's messages to SQLite.
    pub fn persist_session(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
        platform: Option<&str>,
        title: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<(), AgentError> {
        self.ensure_db()?;

        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        let now = Utc::now().to_rfc3339();

        // Upsert session record
        conn.execute(
            "INSERT INTO sessions (id, model, platform, created_at, updated_at, title, message_count, system_prompt)
             VALUES (?1, COALESCE(?2, 'unknown'), COALESCE(?3, 'cli'), ?4, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                updated_at = ?4,
                model = COALESCE(?2, sessions.model),
                platform = COALESCE(?3, sessions.platform),
                message_count = ?6,
                title = COALESCE(?5, sessions.title),
                system_prompt = COALESCE(?7, sessions.system_prompt)",
            rusqlite::params![
                session_id,
                model,
                platform,
                now,
                title,
                messages.len() as i64,
                system_prompt,
            ],
        )
        .map_err(|e| AgentError::Io(format!("Failed to upsert session: {e}")))?;

        // Batch insert messages
        self.flush_messages_to_session_db(&conn, session_id, messages)?;

        Ok(())
    }

    /// Update the persisted model for an existing session after a mid-session switch.
    ///
    /// This intentionally does not create a new session row: callers use it as a
    /// best-effort dashboard/search metadata refresh for sessions that have
    /// already been persisted.
    pub fn update_session_model(&self, session_id: &str, model: &str) -> Result<bool, AgentError> {
        let model = model.trim();
        if session_id.trim().is_empty() || model.is_empty() {
            return Ok(false);
        }
        if !self.db_path.exists() {
            return Ok(false);
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        match conn.execute(
            "UPDATE sessions SET model = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![model, Utc::now().to_rfc3339(), session_id],
        ) {
            Ok(changed) => Ok(changed > 0),
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("no such table") =>
            {
                Ok(false)
            }
            Err(e) => Err(AgentError::Io(format!(
                "Failed to update session model: {e}"
            ))),
        }
    }

    /// Read the persisted model metadata for a session without creating a database.
    pub fn get_session_model(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        if session_id.trim().is_empty() || !self.db_path.exists() {
            return Ok(None);
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let mut stmt = match conn.prepare("SELECT model FROM sessions WHERE id = ?1") {
            Ok(stmt) => stmt,
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("no such table") =>
            {
                return Ok(None);
            }
            Err(e) => {
                return Err(AgentError::Io(format!(
                    "Failed to prepare session model query: {e}"
                )));
            }
        };
        match stmt.query_row(rusqlite::params![session_id], |r| {
            r.get::<_, Option<String>>(0)
        }) {
            Ok(model) => Ok(model.filter(|value| !value.trim().is_empty())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("Failed to read session model: {e}"))),
        }
    }

    /// Soft-delete the target user turn and all later active rows.
    ///
    /// `user_turns_back = 1` targets the latest active user message. Larger
    /// counts walk farther back and clamp to the oldest active user turn.
    pub fn rewind_active_user_turns(
        &self,
        session_id: &str,
        user_turns_back: usize,
    ) -> Result<Option<RewindOutcome>, AgentError> {
        let session_id = session_id.trim();
        if session_id.is_empty() || !self.db_path.exists() {
            return Ok(None);
        }
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        let active_user_rows = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, content FROM messages
                     WHERE session_id = ?1 AND role = 'user' AND active = 1
                     ORDER BY id ASC",
                )
                .map_err(|e| {
                    AgentError::Io(format!("Failed to prepare rewind target query: {e}"))
                })?;
            let rows = stmt
                .query_map(rusqlite::params![session_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|e| AgentError::Io(format!("Failed to query rewind targets: {e}")))?;
            let mut rows_out = Vec::new();
            for row in rows {
                rows_out.push(row.map_err(|e| {
                    AgentError::Io(format!("Failed to read rewind target row: {e}"))
                })?);
            }
            rows_out
        };
        if active_user_rows.is_empty() {
            return Ok(None);
        }
        let count = user_turns_back.max(1);
        let target_index = active_user_rows.len().saturating_sub(count);
        let (target_message_id, target_content) = active_user_rows[target_index].clone();

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| AgentError::Io(format!("Failed to open rewind transaction: {e}")))?;
        let inactive_count = tx
            .execute(
                "UPDATE messages
                 SET active = 0
                 WHERE session_id = ?1 AND active = 1 AND id >= ?2",
                rusqlite::params![session_id, target_message_id],
            )
            .map_err(|e| AgentError::Io(format!("Failed to soft-delete rewound rows: {e}")))?
            as u64;
        let active_message_count: u64 = tx
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND active = 1",
                rusqlite::params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to count active messages: {e}")))?
            .max(0) as u64;
        let rewind_count: u64 = tx
            .query_row(
                "SELECT COALESCE(rewind_count, 0) + 1 FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(1)
            .max(0) as u64;
        tx.execute(
            "UPDATE sessions
             SET rewind_count = ?1, message_count = ?2, updated_at = ?3
             WHERE id = ?4",
            rusqlite::params![
                rewind_count as i64,
                active_message_count as i64,
                Utc::now().to_rfc3339(),
                session_id
            ],
        )
        .map_err(|e| AgentError::Io(format!("Failed to update rewound session row: {e}")))?;
        tx.commit()
            .map_err(|e| AgentError::Io(format!("Failed to commit rewind transaction: {e}")))?;

        Ok(Some(RewindOutcome {
            target_message_id,
            target_content,
            inactive_count,
            active_message_count,
            rewind_count,
        }))
    }

    /// List active user messages newest-first for rewind picker surfaces.
    pub fn list_recent_user_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<UserMessageRef>, AgentError> {
        let session_id = session_id.trim();
        if session_id.is_empty() || limit == 0 || !self.db_path.exists() {
            return Ok(Vec::new());
        }
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, content FROM messages
                 WHERE session_id = ?1 AND role = 'user' AND active = 1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare recent user query: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![session_id, limit as i64], |row| {
                Ok(UserMessageRef {
                    id: row.get(0)?,
                    content: row.get(1)?,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query recent user messages: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| {
                    AgentError::Io(format!("Failed to read recent user message: {e}"))
                })?,
            );
        }
        Ok(out)
    }

    /// Restore inactive rows at or after a message id.
    pub fn restore_rewound_since(
        &self,
        session_id: &str,
        since_message_id: i64,
    ) -> Result<u64, AgentError> {
        let session_id = session_id.trim();
        if session_id.is_empty() || since_message_id <= 0 || !self.db_path.exists() {
            return Ok(0);
        }
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let restored = conn
            .execute(
                "UPDATE messages
                 SET active = 1
                 WHERE session_id = ?1 AND active = 0 AND id >= ?2",
                rusqlite::params![session_id, since_message_id],
            )
            .map_err(|e| AgentError::Io(format!("Failed to restore rewound rows: {e}")))?
            as u64;
        let active_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND active = 1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        conn.execute(
            "UPDATE sessions SET message_count = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![active_count, Utc::now().to_rfc3339(), session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to update restored session row: {e}")))?;
        Ok(restored)
    }

    /// Batch insert messages into the database for FTS5 indexing.
    fn flush_messages_to_session_db(
        &self,
        conn: &rusqlite::Connection,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), AgentError> {
        // Replace the live transcript while preserving inactive rewind audit rows.
        Self::delete_fts_rows_for_session(conn, session_id)?;
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND active = 1",
            rusqlite::params![session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to clear old messages: {e}")))?;

        let now = Utc::now().to_rfc3339();

        let mut stmt = conn
            .prepare(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, reasoning_content, created_at, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare insert: {e}")))?;

        for msg in messages {
            let role = match msg.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };

            let tool_calls_json = msg
                .tool_calls
                .as_ref()
                .map(|tc| serde_json::to_string(tc).unwrap_or_default());

            stmt.execute(rusqlite::params![
                session_id,
                role,
                msg.content.as_deref(),
                msg.tool_call_id.as_deref(),
                tool_calls_json.as_deref(),
                msg.reasoning_content.as_deref(),
                now,
            ])
            .map_err(|e| AgentError::Io(format!("Failed to insert message: {e}")))?;
        }

        Ok(())
    }

    /// Save a human-readable session log as markdown.
    pub fn save_session_log(
        &self,
        session_id: &str,
        messages: &[Message],
        model: Option<&str>,
    ) -> Result<PathBuf, AgentError> {
        std::fs::create_dir_all(&self.sessions_dir)
            .map_err(|e| AgentError::Io(format!("Failed to create sessions dir: {e}")))?;

        let timestamp = Utc::now().format("%Y-%m-%d_%H%M%S");
        let filename = format!("{timestamp}-{session_id}.md");
        let path = self.sessions_dir.join(&filename);

        let mut content = String::new();
        content.push_str(&format!("# Session: {session_id}\n\n"));
        if let Some(m) = model {
            content.push_str(&format!("Model: {m}\n"));
        }
        content.push_str(&format!(
            "Date: {}\n\n---\n\n",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        ));

        for msg in messages {
            let role_label = match msg.role {
                MessageRole::System => "🔧 System",
                MessageRole::User => "👤 User",
                MessageRole::Assistant => "🤖 Assistant",
                MessageRole::Tool => "🔨 Tool",
            };

            content.push_str(&format!("### {role_label}\n\n"));

            if let Some(ref text) = msg.content {
                content.push_str(text);
                content.push_str("\n\n");
            }

            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    content.push_str(&format!(
                        "**Tool call:** `{}({})`\n\n",
                        tc.function.name, tc.function.arguments
                    ));
                }
            }
        }

        std::fs::write(&path, &content)
            .map_err(|e| AgentError::Io(format!("Failed to write session log: {e}")))?;

        Ok(path)
    }

    /// Save messages in trajectory format for RL training.
    pub fn save_trajectory(
        &self,
        session_id: &str,
        messages: &[Message],
        user_query: &str,
        completed: bool,
    ) -> Result<PathBuf, AgentError> {
        std::fs::create_dir_all(&self.trajectories_dir)
            .map_err(|e| AgentError::Io(format!("Failed to create trajectories dir: {e}")))?;

        let timestamp = Utc::now().format("%Y-%m-%d_%H%M%S");
        let filename = format!("{timestamp}-{session_id}.json");
        let path = self.trajectories_dir.join(&filename);

        let trajectory = serde_json::json!({
            "session_id": session_id,
            "user_query": user_query,
            "completed": completed,
            "timestamp": Utc::now().to_rfc3339(),
            "messages": messages,
            "turn_count": messages.iter().filter(|m| m.role == MessageRole::Assistant).count(),
        });

        let json_str = serde_json::to_string_pretty(&trajectory)
            .map_err(|e| AgentError::Io(format!("Failed to serialize trajectory: {e}")))?;

        std::fs::write(&path, &json_str)
            .map_err(|e| AgentError::Io(format!("Failed to write trajectory: {e}")))?;

        Ok(path)
    }

    /// Load persisted full system prompt for prefix-cache continuity (Python `sessions.system_prompt`).
    pub fn get_system_prompt(&self, session_id: &str) -> Result<Option<String>, AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT system_prompt FROM sessions WHERE id = ?1")
            .map_err(|e| AgentError::Io(format!("Failed to prepare query: {e}")))?;
        match stmt.query_row(rusqlite::params![session_id], |r| {
            r.get::<_, Option<String>>(0)
        }) {
            Ok(s) => Ok(s.filter(|t| !t.trim().is_empty())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("Failed to read system_prompt: {e}"))),
        }
    }

    /// Load a previous session from SQLite.
    pub fn load_session(&self, session_id: &str) -> Result<Vec<Message>, AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT role, content, tool_call_id, tool_calls, reasoning_content
                 FROM messages
                 WHERE session_id = ?1 AND active = 1
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare query: {e}")))?;

        let messages = stmt
            .query_map(rusqlite::params![session_id], |row| {
                let role_str: String = row.get(0)?;
                let content: Option<String> = row.get(1)?;
                let tool_call_id: Option<String> = row.get(2)?;
                let tool_calls_json: Option<String> = row.get(3)?;
                let reasoning_content: Option<String> = row.get(4)?;

                let role = match role_str.as_str() {
                    "system" => MessageRole::System,
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    _ => MessageRole::User,
                };

                let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());

                Ok(Message {
                    role,
                    content,
                    tool_calls,
                    tool_call_id,
                    name: None,
                    reasoning_content,
                    cache_control: None,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query messages: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read messages: {e}")))?;

        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::Message;
    use rusqlite::params;

    fn mark_session_old(sp: &SessionPersistence, session_id: &str, days_old: i64) {
        let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
        let ts = (Utc::now() - ChronoDuration::days(days_old)).to_rfc3339();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![ts, session_id],
        )
        .expect("update updated_at");
    }

    fn grow_sessions_wal(
        sp: &SessionPersistence,
    ) -> (rusqlite::Connection, std::path::PathBuf, u64) {
        sp.ensure_db().unwrap();
        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        conn.execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS wal_growth (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                payload TEXT NOT NULL
            )",
            [],
        )
        .unwrap();

        let payload = "x".repeat(64 * 1024);
        let tx = conn.unchecked_transaction().unwrap();
        for _ in 0..96 {
            tx.execute(
                "INSERT INTO wal_growth (payload) VALUES (?1)",
                params![&payload],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let wal_path = std::path::PathBuf::from(format!("{}-wal", sp.db_path.display()));
        let wal_len = std::fs::metadata(&wal_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        assert!(
            wal_len > 0,
            "test setup should create a non-empty sessions.db-wal sidecar"
        );
        (conn, wal_path, wal_len)
    }

    fn wal_len(path: &Path) -> u64 {
        std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
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

        sp.persist_session(
            "test-session-1",
            &messages,
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
        assert_eq!(loaded[0].role, MessageRole::System);
        assert_eq!(loaded[1].content.as_deref(), Some("Hello"));
        assert_eq!(loaded[2].content.as_deref(), Some("Hi there!"));
        assert_eq!(
            loaded[2].reasoning_content.as_deref(),
            Some("provider scratchpad")
        );
    }

    #[test]
    fn test_migrates_reasoning_content_column_for_legacy_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("sessions.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                title TEXT,
                message_count INTEGER DEFAULT 0
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                created_at TEXT NOT NULL
            );",
        )
        .unwrap();
        drop(conn);

        let sp = SessionPersistence::new(tmp.path());
        let mut assistant = Message::assistant("legacy");
        assistant.reasoning_content = Some("legacy-think".to_string());
        sp.persist_session("legacy-migrate", &[assistant], None, None, None, None)
            .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let mut sessions_stmt = conn
            .prepare("PRAGMA table_info(sessions)")
            .expect("sessions pragma prepare");
        let session_cols = sessions_stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for column in [
            "system_prompt",
            "parent_session_id",
            "model_config",
            "end_reason",
            "ended_at",
        ] {
            assert!(
                session_cols.iter().any(|name| name == column),
                "missing migrated sessions.{column}"
            );
        }
        let mut stmt = conn
            .prepare("PRAGMA table_info(messages)")
            .expect("pragma prepare");
        let has_reasoning_col = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .iter()
            .any(|name| name == "reasoning_content");
        assert!(has_reasoning_col);

        let loaded = sp.load_session("legacy-migrate").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].reasoning_content.as_deref(), Some("legacy-think"));
    }

    #[test]
    fn test_save_session_log() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![Message::user("What is 2+2?"), Message::assistant("4")];

        let path = sp
            .save_session_log("log-test", &messages, Some("gpt-4o"))
            .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("What is 2+2?"));
        assert!(content.contains("👤 User"));
        assert!(content.contains("🤖 Assistant"));
    }

    #[test]
    fn test_save_trajectory() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![
            Message::user("Build a website"),
            Message::assistant("Sure, I'll help with that."),
        ];

        let path = sp
            .save_trajectory("traj-test", &messages, "Build a website", true)
            .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["completed"], true);
        assert_eq!(parsed["user_query"], "Build a website");
    }

    #[test]
    fn test_load_nonexistent_session() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.ensure_db().unwrap();

        let loaded = sp.load_session("nonexistent").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_persist_replaces_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages1 = vec![Message::user("First")];
        sp.persist_session("replace-test", &messages1, None, None, None, None)
            .unwrap();

        let messages2 = vec![
            Message::user("First"),
            Message::assistant("Response"),
            Message::user("Second"),
        ];
        sp.persist_session("replace-test", &messages2, None, None, None, None)
            .unwrap();

        let loaded = sp.load_session("replace-test").unwrap();
        assert_eq!(loaded.len(), 3);
    }

    #[test]
    fn test_persist_session_updates_existing_model() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("hello")];

        sp.persist_session(
            "model-update",
            &messages,
            Some("openai:gpt-4o"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
        sp.persist_session(
            "model-update",
            &messages,
            Some("nous:hermes-4"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        let model: String = conn
            .query_row(
                "SELECT model FROM sessions WHERE id='model-update'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(model, "nous:hermes-4");
    }

    #[test]
    fn test_update_session_model_only_updates_existing_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("hello")];

        assert!(!sp
            .update_session_model("missing-session", "openai:gpt-4o")
            .unwrap());

        sp.persist_session(
            "existing-session",
            &messages,
            Some("openai:gpt-4o"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
        assert!(sp
            .update_session_model("existing-session", "anthropic:claude-sonnet")
            .unwrap());

        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        let model: String = conn
            .query_row(
                "SELECT model FROM sessions WHERE id='existing-session'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(model, "anthropic:claude-sonnet");
    }

    #[test]
    fn test_rewind_soft_deletes_n_user_turns_and_loads_only_active_rows() {
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
        sp.persist_session("rewind-n", &messages, None, None, None, None)
            .unwrap();

        let recent = sp.list_recent_user_messages("rewind-n", 2).unwrap();
        assert_eq!(
            recent
                .iter()
                .filter_map(|row| row.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["question 3", "question 2"]
        );

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

        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        let inactive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id='rewind-n' AND active=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(inactive, 4);
        let rewind_count: i64 = conn
            .query_row(
                "SELECT rewind_count FROM sessions WHERE id='rewind-n'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rewind_count, 1);

        assert_eq!(
            sp.restore_rewound_since("rewind-n", outcome.target_message_id)
                .unwrap(),
            4
        );
        assert_eq!(sp.load_session("rewind-n").unwrap().len(), messages.len());
    }

    #[test]
    fn test_persist_session_preserves_inactive_rewind_audit_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![
            Message::user("keep"),
            Message::assistant("kept"),
            Message::user("rewind me"),
            Message::assistant("rewound"),
        ];
        sp.persist_session("audit-preserve", &messages, None, None, None, None)
            .unwrap();
        sp.rewind_active_user_turns("audit-preserve", 1)
            .unwrap()
            .unwrap();

        sp.persist_session(
            "audit-preserve",
            &[Message::user("keep"), Message::assistant("replacement")],
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        let inactive: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id='audit-preserve' AND active=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(inactive, 2);
        assert_eq!(sp.load_session("audit-preserve").unwrap().len(), 2);
    }

    #[test]
    fn test_replacing_messages_clears_old_fts_rows_when_available() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        sp.persist_session(
            "fts-replace",
            &[Message::user("needlebefore")],
            None,
            None,
            None,
            None,
        )
        .unwrap();
        sp.persist_session(
            "fts-replace",
            &[Message::user("needleafter")],
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        if SessionPersistence::fts_table_exists(&conn).unwrap() {
            let old_hits: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'needlebefore'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let new_hits: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'needleafter'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(old_hits, 0);
            assert_eq!(new_hits, 1);
        }
    }

    #[test]
    fn test_delete_fts_rows_ignores_missing_fts_table() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                created_at TEXT NOT NULL
            );",
        )
        .unwrap();

        SessionPersistence::delete_fts_rows_for_session(&conn, "no-fts").unwrap();
    }

    #[test]
    fn test_fts_unavailable_error_classifier_matches_missing_modules() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let err = conn
            .execute_batch(
                "CREATE VIRTUAL TABLE broken_fts USING definitely_missing_module(content);",
            )
            .expect_err("missing virtual table module should fail");
        assert!(SessionPersistence::is_fts5_unavailable_error(&err));
    }

    #[test]
    fn test_state_meta_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        assert_eq!(sp.get_meta("nope").unwrap(), None);
        sp.set_meta("k1", "v1").unwrap();
        assert_eq!(sp.get_meta("k1").unwrap().as_deref(), Some("v1"));
        sp.set_meta("k1", "v2").unwrap();
        assert_eq!(sp.get_meta("k1").unwrap().as_deref(), Some("v2"));
    }

    #[test]
    fn test_truncate_wal_checkpoint_shrinks_sessions_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let (_conn, wal_path, before) = grow_sessions_wal(&sp);

        sp.truncate_wal_checkpoint().unwrap();

        assert_eq!(
            wal_len(&wal_path),
            0,
            "TRUNCATE checkpoint should shrink WAL from {before} bytes"
        );
    }

    #[test]
    fn test_auto_maintenance_truncates_wal_after_meta_write() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let (_conn, wal_path, before) = grow_sessions_wal(&sp);

        let result = sp.maybe_auto_prune_and_vacuum(90, 0, false);

        assert!(!result.skipped);
        assert_eq!(result.pruned, 0);
        assert!(result.error.is_none());
        assert_eq!(
            wal_len(&wal_path),
            0,
            "auto-maintenance should truncate WAL from {before} bytes"
        );
    }

    #[test]
    fn test_auto_maintenance_prunes_and_vacuums() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let old_messages = vec![Message::user("old"), Message::assistant("done")];
        let new_messages = vec![Message::user("fresh"), Message::assistant("ok")];

        sp.persist_session("old-1", &old_messages, None, None, None, None)
            .unwrap();
        sp.persist_session("old-2", &old_messages, None, None, None, None)
            .unwrap();
        sp.persist_session("fresh-1", &new_messages, None, None, None, None)
            .unwrap();
        mark_session_old(&sp, "old-1", 100);
        mark_session_old(&sp, "old-2", 100);

        let result = sp.maybe_auto_prune_and_vacuum(90, 24, true);
        assert!(!result.skipped);
        assert_eq!(result.pruned, 2);
        assert!(result.vacuumed);
        assert!(result.error.is_none());

        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn test_auto_maintenance_respects_interval_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("old"), Message::assistant("done")];

        sp.persist_session("old-1", &messages, None, None, None, None)
            .unwrap();
        mark_session_old(&sp, "old-1", 100);
        let first = sp.maybe_auto_prune_and_vacuum(90, 24, false);
        assert!(!first.skipped);
        assert_eq!(first.pruned, 1);

        sp.persist_session("old-2", &messages, None, None, None, None)
            .unwrap();
        mark_session_old(&sp, "old-2", 100);
        let second = sp.maybe_auto_prune_and_vacuum(90, 24, false);
        assert!(second.skipped);
        assert_eq!(second.pruned, 0);

        let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
        let still_there: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions WHERE id='old-2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(still_there, 1);
    }

    #[test]
    fn test_auto_maintenance_no_prunable_rows_skips_vacuum() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("fresh"), Message::assistant("ok")];
        sp.persist_session("fresh-1", &messages, None, None, None, None)
            .unwrap();

        let result = sp.maybe_auto_prune_and_vacuum(90, 24, true);
        assert!(!result.skipped);
        assert_eq!(result.pruned, 0);
        assert!(!result.vacuumed);
        assert!(sp.get_meta("last_auto_prune").unwrap().is_some());
    }

    #[test]
    fn test_auto_maintenance_corrupt_marker_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("old"), Message::assistant("done")];
        sp.persist_session("old-1", &messages, None, None, None, None)
            .unwrap();
        mark_session_old(&sp, "old-1", 100);
        sp.set_meta("last_auto_prune", "not-a-timestamp").unwrap();

        let result = sp.maybe_auto_prune_and_vacuum(90, 24, false);
        assert!(!result.skipped);
        assert_eq!(result.pruned, 1);
    }
}
