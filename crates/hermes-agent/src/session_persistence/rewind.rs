//! Soft rewind / restore primitives (`messages.active` parity).

use std::sync::{Arc, Mutex};

use chrono::Utc;
use hermes_core::AgentError;
use rusqlite::{Connection, params};

use super::schema::table_has_column_pub;
use super::write;

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

fn touch_session_counts(
    c: &Connection,
    session_id: &str,
    message_count: i64,
    rewind_count: Option<i64>,
) -> Result<(), AgentError> {
    let now = Utc::now().to_rfc3339();
    if let Some(rc) = rewind_count {
        if table_has_column_pub(c, "sessions", "updated_at") {
            c.execute(
                "UPDATE sessions SET rewind_count = ?1, message_count = ?2, updated_at = ?3 WHERE id = ?4",
                params![rc, message_count, now, session_id],
            )
        } else {
            c.execute(
                "UPDATE sessions SET rewind_count = ?1, message_count = ?2 WHERE id = ?3",
                params![rc, message_count, session_id],
            )
        }
        .map_err(|e| AgentError::Io(format!("Failed to update rewound session row: {e}")))?;
    } else if table_has_column_pub(c, "sessions", "updated_at") {
        c.execute(
            "UPDATE sessions SET message_count = ?1, updated_at = ?2 WHERE id = ?3",
            params![message_count, now, session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to update restored session row: {e}")))?;
    } else {
        c.execute(
            "UPDATE sessions SET message_count = ?1 WHERE id = ?2",
            params![message_count, session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to update restored session row: {e}")))?;
    }
    Ok(())
}

/// Soft-delete the target user turn and all later active rows.
pub fn rewind_active_user_turns(
    conn: &Arc<Mutex<Connection>>,
    session_id: &str,
    user_turns_back: usize,
) -> Result<Option<RewindOutcome>, AgentError> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Ok(None);
    }
    {
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        if !table_has_column_pub(&guard, "messages", "active") {
            return Ok(None);
        }
    }

    let active_user_rows = {
        let guard = conn
            .lock()
            .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
        let mut stmt = guard
            .prepare(
                "SELECT id, content FROM messages
                 WHERE session_id = ?1 AND role = 'user' AND active = 1
                 ORDER BY id ASC",
            )
            .map_err(|e| AgentError::Io(format!("Failed to prepare rewind target query: {e}")))?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| AgentError::Io(format!("Failed to query rewind targets: {e}")))?;
        let mut rows_out = Vec::new();
        for row in rows {
            rows_out.push(
                row.map_err(|e| AgentError::Io(format!("Failed to read rewind target row: {e}")))?,
            );
        }
        rows_out
    };

    if active_user_rows.is_empty() {
        return Ok(None);
    }
    let count = user_turns_back.max(1);
    let target_index = active_user_rows.len().saturating_sub(count);
    let (target_message_id, target_content) = active_user_rows[target_index].clone();

    let (inactive_count, active_message_count, rewind_count) =
        write::execute_write(conn, |c| {
            let inactive_count = c
                .execute(
                    "UPDATE messages
                 SET active = 0
                 WHERE session_id = ?1 AND active = 1 AND id >= ?2",
                    params![session_id, target_message_id],
                )
                .map_err(|e| AgentError::Io(format!("Failed to soft-delete rewound rows: {e}")))?
                as u64;
            let active_message_count: u64 = c
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND active = 1",
                    params![session_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| AgentError::Io(format!("Failed to count active messages: {e}")))?
                .max(0) as u64;
            let rewind_count: u64 = c
                .query_row(
                    "SELECT COALESCE(rewind_count, 0) + 1 FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(1)
                .max(0) as u64;
            touch_session_counts(
                c,
                session_id,
                active_message_count as i64,
                Some(rewind_count as i64),
            )?;
            Ok((inactive_count, active_message_count, rewind_count))
        })?;

    Ok(Some(RewindOutcome {
        target_message_id,
        target_content,
        inactive_count,
        active_message_count,
        rewind_count,
    }))
}

pub fn list_recent_user_messages(
    conn: &Arc<Mutex<Connection>>,
    session_id: &str,
    limit: usize,
) -> Result<Vec<UserMessageRef>, AgentError> {
    let session_id = session_id.trim();
    if session_id.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    if !table_has_column_pub(&guard, "messages", "active") {
        return Ok(Vec::new());
    }
    let mut stmt = guard
        .prepare(
            "SELECT id, content FROM messages
             WHERE session_id = ?1 AND role = 'user' AND active = 1
             ORDER BY id DESC
             LIMIT ?2",
        )
        .map_err(|e| AgentError::Io(format!("Failed to prepare recent user query: {e}")))?;
    let rows = stmt
        .query_map(params![session_id, limit as i64], |row| {
            Ok(UserMessageRef {
                id: row.get(0)?,
                content: row.get(1)?,
            })
        })
        .map_err(|e| AgentError::Io(format!("Failed to query recent user messages: {e}")))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("Failed to read recent user message: {e}")))
}

pub fn restore_rewound_since(
    conn: &Arc<Mutex<Connection>>,
    session_id: &str,
    since_message_id: i64,
) -> Result<u64, AgentError> {
    let session_id = session_id.trim();
    if session_id.is_empty() || since_message_id <= 0 {
        return Ok(0);
    }
    write::execute_write(conn, |c| {
        if !table_has_column_pub(c, "messages", "active") {
            return Ok(0);
        }
        let restored = c
            .execute(
                "UPDATE messages
                 SET active = 1
                 WHERE session_id = ?1 AND active = 0 AND id >= ?2",
                params![session_id, since_message_id],
            )
            .map_err(|e| AgentError::Io(format!("Failed to restore rewound rows: {e}")))?
            as u64;
        let active_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND active = 1",
                params![session_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        touch_session_counts(c, session_id, active_count, None)?;
        Ok(restored)
    })
}
