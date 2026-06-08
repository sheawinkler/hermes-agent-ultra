//! FTS optimize and WAL TRUNCATE maintenance (main `session_persistence.rs` parity).

use std::sync::{Arc, Mutex};

use hermes_core::AgentError;
use rusqlite::Connection;

const FTS_TABLES: &[&str] = &["messages_fts", "messages_fts_trigram"];

fn is_fts5_unavailable_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("no such module")
        || lower.contains("unknown tokenizer")
        || lower.contains("fts5")
}

fn is_fts5_unavailable_error(error: &rusqlite::Error) -> bool {
    is_fts5_unavailable_message(&error.to_string())
}

pub fn named_fts_table_exists(conn: &Connection, table: &str) -> Result<bool, AgentError> {
    if !FTS_TABLES.contains(&table) {
        return Err(AgentError::Config(format!(
            "Unsupported FTS table name for state db maintenance: {table}"
        )));
    }
    let sql = format!("SELECT 1 FROM {table} LIMIT 0");
    match conn.prepare(&sql) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table") || is_fts5_unavailable_message(&message) =>
        {
            Ok(false)
        }
        Err(err) if is_fts5_unavailable_error(&err) => Ok(false),
        Err(err) => Err(AgentError::Io(format!(
            "Failed to inspect {table} availability: {err}"
        ))),
    }
}

pub fn fts_index_count(conn: &Connection) -> Result<u32, AgentError> {
    let mut count = 0u32;
    for table in FTS_TABLES {
        if named_fts_table_exists(conn, table)? {
            count += 1;
        }
    }
    Ok(count)
}

pub fn optimize_fts_on_conn(conn: &Connection) -> Result<u32, AgentError> {
    let mut optimized = 0u32;
    for table in FTS_TABLES {
        if !named_fts_table_exists(conn, table)? {
            continue;
        }
        let sql = format!("INSERT INTO {table}({table}) VALUES('optimize')");
        match conn.execute(&sql, []) {
            Ok(_) => optimized += 1,
            Err(err) => {
                tracing::warn!("FTS optimize failed for {}: {}", table, err);
            }
        }
    }
    Ok(optimized)
}

pub fn truncate_wal_checkpoint(conn: &Arc<Mutex<Connection>>) -> Result<(), AgentError> {
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = guard
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|e| AgentError::Io(format!("Failed to checkpoint state db WAL: {e}")))?;

    if busy > 0 {
        tracing::warn!(
            "state.db WAL checkpoint could not truncate immediately: busy={}, log_frames={}, checkpointed_frames={}",
            busy,
            log_frames,
            checkpointed_frames
        );
    } else if log_frames > 0 {
        tracing::debug!(
            "state.db WAL checkpoint truncated {} frame(s); checkpointed={}",
            log_frames,
            checkpointed_frames
        );
    }
    Ok(())
}
