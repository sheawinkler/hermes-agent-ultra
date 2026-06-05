//! SQLite schema initialization and declarative column reconciliation.
//!
//! Mirrors Python `hermes_state.SCHEMA_SQL`, FTS tables, and `_reconcile_columns`.

use hermes_core::AgentError;
use rusqlite::Connection;
use tracing::warn;

pub const SCHEMA_VERSION: i64 = 14;

const BASE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL DEFAULT 'cli',
    user_id TEXT,
    model TEXT,
    model_config TEXT,
    system_prompt TEXT,
    parent_session_id TEXT,
    started_at REAL NOT NULL DEFAULT 0,
    ended_at REAL,
    end_reason TEXT,
    message_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0,
    cache_write_tokens INTEGER DEFAULT 0,
    reasoning_tokens INTEGER DEFAULT 0,
    cwd TEXT,
    billing_provider TEXT,
    billing_base_url TEXT,
    billing_mode TEXT,
    estimated_cost_usd REAL,
    actual_cost_usd REAL,
    cost_status TEXT,
    cost_source TEXT,
    pricing_version TEXT,
    title TEXT,
    api_call_count INTEGER DEFAULT 0,
    handoff_state TEXT,
    handoff_platform TEXT,
    handoff_error TEXT,
    FOREIGN KEY (parent_session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role TEXT NOT NULL,
    content TEXT,
    tool_call_id TEXT,
    tool_calls TEXT,
    tool_name TEXT,
    timestamp REAL NOT NULL DEFAULT 0,
    token_count INTEGER,
    finish_reason TEXT,
    reasoning TEXT,
    reasoning_content TEXT,
    reasoning_details TEXT,
    codex_reasoning_items TEXT,
    codex_message_items TEXT,
    platform_message_id TEXT,
    observed INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS state_meta (
    key TEXT PRIMARY KEY,
    value TEXT
);

CREATE TABLE IF NOT EXISTS compression_locks (
    session_id TEXT PRIMARY KEY,
    holder TEXT NOT NULL,
    acquired_at REAL NOT NULL,
    expires_at REAL NOT NULL
);
"#;

/// Core indexes — must run after [`reconcile_table`] on legacy DBs missing columns like `source`.
const SESSION_INDEXES_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_sessions_source ON sessions(source);
CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_compression_locks_expires ON compression_locks(expires_at);
"#;

const FTS_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(content);

CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_update AFTER UPDATE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
    INSERT INTO messages_fts(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;
"#;

const FTS_TRIGRAM_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts_trigram USING fts5(
    content,
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS messages_fts_trigram_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts_trigram(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_trigram_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts_trigram WHERE rowid = old.id;
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_trigram_update AFTER UPDATE ON messages BEGIN
    DELETE FROM messages_fts_trigram WHERE rowid = old.id;
    INSERT INTO messages_fts_trigram(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;
"#;

const GATEWAY_INDEX_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS gateway_session_index (
    session_key TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL
);
"#;

const LEGACY_SESSION_COLUMNS: &[(&str, &str)] = &[
    ("platform", "TEXT DEFAULT 'cli'"),
    ("created_at", "TEXT"),
    ("updated_at", "TEXT"),
];

const SESSIONS_COLUMNS: &[(&str, &str)] = &[
    ("source", "TEXT NOT NULL DEFAULT 'cli'"),
    ("user_id", "TEXT"),
    ("model", "TEXT"),
    ("model_config", "TEXT"),
    ("system_prompt", "TEXT"),
    ("parent_session_id", "TEXT"),
    ("started_at", "REAL NOT NULL DEFAULT 0"),
    ("ended_at", "REAL"),
    ("end_reason", "TEXT"),
    ("message_count", "INTEGER DEFAULT 0"),
    ("tool_call_count", "INTEGER DEFAULT 0"),
    ("input_tokens", "INTEGER DEFAULT 0"),
    ("output_tokens", "INTEGER DEFAULT 0"),
    ("cache_read_tokens", "INTEGER DEFAULT 0"),
    ("cache_write_tokens", "INTEGER DEFAULT 0"),
    ("reasoning_tokens", "INTEGER DEFAULT 0"),
    ("cwd", "TEXT"),
    ("billing_provider", "TEXT"),
    ("billing_base_url", "TEXT"),
    ("billing_mode", "TEXT"),
    ("estimated_cost_usd", "REAL"),
    ("actual_cost_usd", "REAL"),
    ("cost_status", "TEXT"),
    ("cost_source", "TEXT"),
    ("pricing_version", "TEXT"),
    ("title", "TEXT"),
    ("api_call_count", "INTEGER DEFAULT 0"),
    ("handoff_state", "TEXT"),
    ("handoff_platform", "TEXT"),
    ("handoff_error", "TEXT"),
];

const MESSAGES_COLUMNS: &[(&str, &str)] = &[
    ("tool_name", "TEXT"),
    ("timestamp", "REAL NOT NULL DEFAULT 0"),
    ("token_count", "INTEGER"),
    ("finish_reason", "TEXT"),
    ("reasoning", "TEXT"),
    ("reasoning_content", "TEXT"),
    ("reasoning_details", "TEXT"),
    ("codex_reasoning_items", "TEXT"),
    ("codex_message_items", "TEXT"),
    ("platform_message_id", "TEXT"),
    ("observed", "INTEGER DEFAULT 0"),
    ("created_at", "TEXT"),
];

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, AgentError> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| AgentError::Io(format!("PRAGMA table_info({table}): {e}")))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| AgentError::Io(format!("PRAGMA table_info rows: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("PRAGMA table_info read: {e}")))?;
    Ok(names.iter().any(|n| n == column))
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> Result<(), AgentError> {
    if table_has_column(conn, table, column)? {
        return Ok(());
    }
    let sql = format!(r#"ALTER TABLE "{table}" ADD COLUMN "{column}" {col_type}"#);
    if let Err(e) = conn.execute(&sql, []) {
        let msg = e.to_string();
        if !msg.contains("duplicate column") {
            return Err(AgentError::Io(format!(
                "Failed to add {table}.{column}: {e}"
            )));
        }
    }
    Ok(())
}

fn reconcile_table(conn: &Connection, table: &str, columns: &[(&str, &str)]) -> Result<(), AgentError> {
    for (col, col_type) in columns {
        add_column_if_missing(conn, table, col, col_type)?;
    }
    Ok(())
}

fn migrate_legacy_sessions(conn: &Connection) -> Result<(), AgentError> {
    reconcile_table(conn, "sessions", LEGACY_SESSION_COLUMNS)?;

    let has_source = table_has_column(conn, "sessions", "source")?;
    let has_platform = table_has_column(conn, "sessions", "platform")?;
    if has_source && has_platform {
        conn.execute(
            "UPDATE sessions SET source = platform WHERE (source IS NULL OR source = '') AND platform IS NOT NULL",
            [],
        )
        .map_err(|e| AgentError::Io(format!("migrate platform→source: {e}")))?;
    }

    if table_has_column(conn, "sessions", "started_at")? {
        if table_has_column(conn, "sessions", "created_at")? {
            conn.execute(
                "UPDATE sessions SET started_at = CAST(strftime('%s', created_at) AS REAL)
                 WHERE (started_at IS NULL OR started_at = 0) AND created_at IS NOT NULL",
                [],
            )
            .ok();
        }
        conn.execute(
            "UPDATE sessions SET started_at = CAST(strftime('%s', 'now') AS REAL)
             WHERE started_at IS NULL OR started_at = 0",
            [],
        )
        .ok();
    }

    if table_has_column(conn, "messages", "timestamp")? {
        if table_has_column(conn, "messages", "created_at")? {
            conn.execute(
                "UPDATE messages SET timestamp = CAST(strftime('%s', created_at) AS REAL)
                 WHERE (timestamp IS NULL OR timestamp = 0) AND created_at IS NOT NULL",
                [],
            )
            .ok();
        }
        conn.execute(
            "UPDATE messages SET timestamp = CAST(strftime('%s', 'now') AS REAL)
             WHERE timestamp IS NULL OR timestamp = 0",
            [],
        )
        .ok();
    }

    Ok(())
}

fn fts_uses_external_content(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='messages_fts'",
        [],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .map(|sql| sql.to_ascii_lowercase().contains("content='messages'"))
    .unwrap_or(false)
}

fn drop_fts_triggers(conn: &Connection) {
    for trigger in [
        "messages_fts_insert",
        "messages_fts_delete",
        "messages_fts_update",
        "messages_fts_trigram_insert",
        "messages_fts_trigram_delete",
        "messages_fts_trigram_update",
        "messages_ai",
        "messages_ad",
        "messages_au",
    ] {
        let _ = conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"));
    }
}

fn sqlite_supports_fts5(conn: &Connection) -> bool {
    match conn.execute_batch(
        "CREATE VIRTUAL TABLE temp._hermes_fts5_probe USING fts5(x);
         DROP TABLE temp._hermes_fts5_probe;",
    ) {
        Ok(()) => true,
        Err(e) => {
            let msg = e.to_string().to_ascii_lowercase();
            if msg.contains("no such module") && msg.contains("fts5") {
                warn!(
                    "SQLite FTS5 unavailable; full-text session search disabled ({e})"
                );
                false
            } else {
                warn!("FTS5 probe failed: {e}");
                false
            }
        }
    }
}

fn rebuild_fts_indexes(conn: &Connection) -> Result<(), AgentError> {
    conn.execute("DELETE FROM messages_fts", [])
        .map_err(|e| AgentError::Io(format!("clear messages_fts: {e}")))?;
    conn.execute(
        "INSERT INTO messages_fts(rowid, content)
         SELECT id,
                COALESCE(content, '') || ' ' || COALESCE(tool_name, '') || ' ' || COALESCE(tool_calls, '')
         FROM messages",
        [],
    )
    .map_err(|e| AgentError::Io(format!("rebuild messages_fts: {e}")))?;

    if table_exists(conn, "messages_fts_trigram")? {
        conn.execute("DELETE FROM messages_fts_trigram", [])
            .map_err(|e| AgentError::Io(format!("clear messages_fts_trigram: {e}")))?;
        conn.execute(
            "INSERT INTO messages_fts_trigram(rowid, content)
             SELECT id,
                    COALESCE(content, '') || ' ' || COALESCE(tool_name, '') || ' ' || COALESCE(tool_calls, '')
             FROM messages",
            [],
        )
        .map_err(|e| AgentError::Io(format!("rebuild messages_fts_trigram: {e}")))?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, AgentError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .map_err(|e| AgentError::Io(format!("table_exists({name}): {e}")))?;
    Ok(count > 0)
}

fn ensure_python_fts(conn: &Connection) -> Result<bool, AgentError> {
    if !sqlite_supports_fts5(conn) {
        drop_fts_triggers(conn);
        return Ok(false);
    }

    let external = fts_uses_external_content(conn);
    if external {
        drop_fts_triggers(conn);
        let _ = conn.execute_batch("DROP TABLE IF EXISTS messages_fts");
        conn.execute(
            "INSERT INTO state_meta (key, value) VALUES ('fts_migrated_v2', 'pending')
             ON CONFLICT(key) DO UPDATE SET value = 'pending'",
            [],
        )
        .ok();
    }

    conn.execute_batch(FTS_SQL)
        .map_err(|e| AgentError::Io(format!("FTS schema: {e}")))?;
    conn.execute_batch(FTS_TRIGRAM_SQL)
        .map_err(|e| AgentError::Io(format!("FTS trigram schema: {e}")))?;

    let needs_rebuild: bool = conn
        .query_row(
            "SELECT value FROM state_meta WHERE key = 'fts_migrated_v2'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map(|v| v == "pending")
        .unwrap_or(false);

    if needs_rebuild || external {
        rebuild_fts_indexes(conn)?;
        conn.execute(
            "INSERT INTO state_meta (key, value) VALUES ('fts_migrated_v2', 'done')
             ON CONFLICT(key) DO UPDATE SET value = 'done'",
            [],
        )
        .map_err(|e| AgentError::Io(format!("record fts migration: {e}")))?;
    }

    Ok(true)
}

fn ensure_schema_version(conn: &Connection) -> Result<(), AgentError> {
    let row: Option<i64> = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
            r.get(0)
        })
        .ok();
    match row {
        None => {
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                rusqlite::params![SCHEMA_VERSION],
            )
            .map_err(|e| AgentError::Io(format!("init schema_version: {e}")))?;
        }
        Some(v) if v < SCHEMA_VERSION => {
            conn.execute(
                "UPDATE schema_version SET version = ?1",
                rusqlite::params![SCHEMA_VERSION],
            )
            .map_err(|e| AgentError::Io(format!("bump schema_version: {e}")))?;
        }
        _ => {}
    }
    Ok(())
}

fn ensure_title_unique_index(conn: &Connection) -> Result<(), AgentError> {
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_title_unique
         ON sessions(title) WHERE title IS NOT NULL",
    )
    .map_err(|e| AgentError::Io(format!("title unique index: {e}")))?;
    Ok(())
}

fn ensure_core_indexes(conn: &Connection) -> Result<(), AgentError> {
    conn.execute_batch(SESSION_INDEXES_SQL)
        .map_err(|e| AgentError::Io(format!("session indexes: {e}")))?;

    if table_has_column(conn, "messages", "platform_message_id")? {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_messages_platform_msg_id
             ON messages(session_id, platform_message_id)
             WHERE platform_message_id IS NOT NULL",
        )
        .map_err(|e| AgentError::Io(format!("platform_message_id index: {e}")))?;
    }
    Ok(())
}

/// Initialize or migrate the database schema to Python `hermes_state` parity.
pub fn init_schema(conn: &Connection) -> Result<bool, AgentError> {
    conn.execute_batch(BASE_SCHEMA_SQL)
        .map_err(|e| AgentError::Io(format!("base schema: {e}")))?;

    reconcile_table(conn, "sessions", SESSIONS_COLUMNS)?;
    reconcile_table(conn, "messages", MESSAGES_COLUMNS)?;
    migrate_legacy_sessions(conn)?;
    ensure_core_indexes(conn)?;

    let fts_enabled = ensure_python_fts(conn)?;

    conn.execute_batch(GATEWAY_INDEX_SQL)
        .map_err(|e| AgentError::Io(format!("gateway_session_index: {e}")))?;

    ensure_schema_version(conn)?;
    ensure_title_unique_index(conn)?;

    Ok(fts_enabled)
}

/// SQL expression for session source (legacy `platform` fallback).
pub fn source_expr() -> &'static str {
    "COALESCE(NULLIF(source, ''), NULLIF(platform, ''), 'cli')"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_schema_creates_core_tables() {
        let conn = Connection::open_in_memory().unwrap();
        let fts = init_schema(&conn).unwrap();
        assert!(fts);
        assert!(table_exists(&conn, "sessions").unwrap());
        assert!(table_exists(&conn, "messages_fts_trigram").unwrap());
    }

    #[test]
    fn init_schema_migrates_legacy_sessions_without_source() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                created_at TEXT NOT NULL
            );",
        )
        .unwrap();

        let fts = init_schema(&conn).unwrap();
        assert!(fts);
        assert!(table_has_column(&conn, "sessions", "source").unwrap());

        hermes_tools::state_db::insert_session_if_missing(
            &conn,
            "legacy-1",
            "cli",
            None,
            None,
            None,
            None,
            1.0,
        )
        .unwrap();

        let created: String = conn
            .query_row(
                "SELECT created_at FROM sessions WHERE id = 'legacy-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!created.is_empty());
    }
}
