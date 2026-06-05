//! SessionDB query and mutation helpers (Python `hermes_state.SessionDB` parity).

use std::collections::HashSet;
use std::sync::Arc;

use hermes_core::{AgentError, Message, MessageRole};
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use super::write::execute_write;

pub const MAX_TITLE_LENGTH: usize = 120;
pub use hermes_tools::state_db::decode_content_preview;

/// Session metadata row (subset of Python session dict).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub source: String,
    pub model: Option<String>,
    pub title: Option<String>,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub end_reason: Option<String>,
    pub message_count: i64,
    pub parent_session_id: Option<String>,
    pub system_prompt: Option<String>,
    pub preview: Option<String>,
    pub last_active: Option<f64>,
    /// Set when `list_sessions_rich` projects a compression root to its tip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_root_id: Option<String>,
}

fn format_preview(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        None
    } else if raw.chars().count() > 60 {
        Some(format!("{}...", raw.chars().take(60).collect::<String>()))
    } else {
        Some(raw.to_string())
    }
}

fn row_to_session(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    let source: Option<String> = row.get("source").ok();
    let platform: Option<String> = row.get("platform").ok();
    let effective = source
        .filter(|s| !s.trim().is_empty())
        .or(platform.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "cli".into());
    Ok(SessionRecord {
        id: row.get("id")?,
        source: effective,
        model: row.get("model")?,
        title: row.get("title")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        end_reason: row.get("end_reason")?,
        message_count: row.get("message_count").unwrap_or(0),
        parent_session_id: row.get("parent_session_id")?,
        system_prompt: row.get("system_prompt")?,
        preview: row.get("_preview_raw").ok(),
        last_active: row.get("last_active").ok(),
        lineage_root_id: None,
    })
}

pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Normalize and validate a session title (Python `SessionDB.sanitize_title`).
pub fn sanitize_title(title: Option<&str>) -> Result<Option<String>, AgentError> {
    let Some(raw) = title.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return Ok(None);
    }
    if cleaned.chars().count() > MAX_TITLE_LENGTH {
        return Err(AgentError::Config(format!(
            "Title too long ({} chars, max {MAX_TITLE_LENGTH})",
            cleaned.chars().count()
        )));
    }
    Ok(Some(cleaned))
}

pub fn create_session(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    source: &str,
    model: Option<&str>,
    parent_session_id: Option<&str>,
    system_prompt: Option<&str>,
    cwd: Option<&str>,
) -> Result<(), AgentError> {
    let sid = session_id.to_string();
    let src = source.to_string();
    let model = model.map(str::to_string);
    let parent = parent_session_id.map(str::to_string);
    let sp = system_prompt.map(str::to_string);
    let cwd = cwd.map(str::to_string);
    let started = now_unix();

    execute_write(conn, |c| {
        hermes_tools::state_db::insert_session_if_missing(
            c,
            &sid,
            &src,
            model.as_deref(),
            parent.as_deref(),
            sp.as_deref(),
            cwd.as_deref(),
            started,
        )
        .map_err(|e| AgentError::Io(e.to_string()))?;
        Ok(())
    })
}

pub fn ensure_session(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    source: &str,
    model: Option<&str>,
) -> Result<(), AgentError> {
    let exists: bool = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?
        .query_row(
            "SELECT 1 FROM sessions WHERE id = ?1",
            params![session_id],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    create_session(conn, session_id, source, model, None, None, None)
}

pub fn get_session(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
) -> Result<Option<SessionRecord>, AgentError> {
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    let mut stmt = guard
        .prepare("SELECT * FROM sessions WHERE id = ?1")
        .map_err(|e| AgentError::Io(format!("get_session prepare: {e}")))?;
    match stmt.query_row(params![session_id], row_to_session) {
        Ok(row) => Ok(Some(row)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(AgentError::Io(format!("get_session: {e}"))),
    }
}

pub fn set_session_title(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    title: Option<&str>,
) -> Result<bool, AgentError> {
    let title = sanitize_title(title)?;
    let sid = session_id.to_string();
    let title_val = title.clone();
    let updated = execute_write(conn, move |c| {
        if let Some(ref t) = title_val {
            let conflict: Option<String> = c
                .query_row(
                    "SELECT id FROM sessions WHERE title = ?1 AND id != ?2",
                    params![t, sid],
                    |r| r.get(0),
                )
                .ok();
            if let Some(other) = conflict {
                return Err(AgentError::Config(format!(
                    "Title '{t}' is already in use by session {other}"
                )));
            }
        }
        let n = c
            .execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2",
                params![title_val, sid],
            )
            .map_err(|e| AgentError::Io(format!("set_session_title: {e}")))?;
        Ok(n > 0)
    })?;
    Ok(updated)
}

pub fn get_session_title(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
) -> Result<Option<String>, AgentError> {
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    match guard.query_row(
        "SELECT title FROM sessions WHERE id = ?1",
        params![session_id],
        |r| r.get::<_, Option<String>>(0),
    ) {
        Ok(t) => Ok(t),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(AgentError::Io(format!("get_session_title: {e}"))),
    }
}

pub fn resolve_session_id(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id_or_prefix: &str,
) -> Result<Option<String>, AgentError> {
    let needle = session_id_or_prefix.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    if let Ok(id) = guard.query_row(
        "SELECT id FROM sessions WHERE id = ?1",
        params![needle],
        |r| r.get::<_, String>(0),
    ) {
        return Ok(Some(id));
    }
    let pattern = format!("{needle}%");
    let mut stmt = guard
        .prepare(
            "SELECT id FROM sessions WHERE id LIKE ?1 ESCAPE '\\' ORDER BY started_at DESC LIMIT 2",
        )
        .map_err(|e| AgentError::Io(format!("resolve_session_id: {e}")))?;
    let rows: Vec<String> = stmt
        .query_map(params![pattern], |r| r.get(0))
        .map_err(|e| AgentError::Io(format!("resolve_session_id query: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("resolve_session_id read: {e}")))?;
    match rows.len() {
        0 => Ok(None),
        1 => Ok(Some(rows[0].clone())),
        _ => Err(AgentError::Config(format!(
            "Session prefix '{needle}' is ambiguous"
        ))),
    }
}

pub fn resolve_session_by_title(
    conn: &Arc<std::sync::Mutex<Connection>>,
    title: &str,
) -> Result<Option<String>, AgentError> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(None);
    }
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    let escaped = title.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("{escaped} #%");
    let numbered: Option<String> = guard
        .query_row(
            "SELECT id FROM sessions WHERE title LIKE ?1 ESCAPE '\\' ORDER BY started_at DESC LIMIT 1",
            params![pattern],
            |r| r.get(0),
        )
        .ok();
    if numbered.is_some() {
        return Ok(numbered);
    }
    match guard.query_row(
        "SELECT id FROM sessions WHERE title = ?1",
        params![title],
        |r| r.get(0),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(AgentError::Io(format!("resolve_session_by_title: {e}"))),
    }
}

pub fn resolve_resume_session_id(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
) -> Result<String, AgentError> {
    if session_id.is_empty() {
        return Ok(session_id.to_string());
    }
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    let has_msgs: bool = guard
        .query_row(
            "SELECT 1 FROM messages WHERE session_id = ?1 LIMIT 1",
            params![session_id],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if has_msgs {
        return Ok(session_id.to_string());
    }

    let mut current = session_id.to_string();
    let mut seen = HashSet::from([current.clone()]);
    for _ in 0..32 {
        let child: Option<String> = guard
            .query_row(
                "SELECT id FROM sessions WHERE parent_session_id = ?1
                 ORDER BY started_at DESC, id DESC LIMIT 1",
                params![current],
                |r| r.get(0),
            )
            .ok();
        let Some(child_id) = child else {
            return Ok(current);
        };
        if seen.contains(&child_id) {
            return Ok(current);
        }
        seen.insert(child_id.clone());
        let child_has_msgs = guard
            .query_row(
                "SELECT 1 FROM messages WHERE session_id = ?1 LIMIT 1",
                params![child_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if child_has_msgs {
            return Ok(child_id);
        }
        current = child_id;
    }
    Ok(current)
}

pub fn end_session(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    end_reason: &str,
) -> Result<(), AgentError> {
    let sid = session_id.to_string();
    let reason = end_reason.to_string();
    let ended = now_unix();
    execute_write(conn, move |c| {
        c.execute(
            "UPDATE sessions SET ended_at = ?1, end_reason = ?2
             WHERE id = ?3 AND ended_at IS NULL",
            params![ended, reason, sid],
        )
        .map_err(|e| AgentError::Io(format!("end_session: {e}")))?;
        Ok(())
    })
}

pub fn reopen_session(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
) -> Result<(), AgentError> {
    let sid = session_id.to_string();
    execute_write(conn, move |c| {
        c.execute(
            "UPDATE sessions SET ended_at = NULL, end_reason = NULL WHERE id = ?1",
            params![sid],
        )
        .map_err(|e| AgentError::Io(format!("reopen_session: {e}")))?;
        Ok(())
    })
}

pub fn update_token_counts(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    update: &hermes_tools::state_db::TokenCountUpdate,
) -> Result<(), AgentError> {
    hermes_tools::state_db::update_token_counts(conn, session_id, update)
        .map_err(|e| AgentError::Io(e.to_string()))
}

pub fn get_compression_tip(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
) -> Result<String, AgentError> {
    hermes_tools::state_db::get_compression_tip(conn, session_id)
        .map_err(|e| AgentError::Io(e.to_string()))
}

pub fn get_session_rich_row(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
) -> Result<Option<SessionRecord>, AgentError> {
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    let mut stmt = guard
        .prepare(
            "SELECT s.*,
                COALESCE(
                    (SELECT SUBSTR(REPLACE(REPLACE(m.content, char(10), ' '), char(13), ' '), 1, 63)
                     FROM messages m
                     WHERE m.session_id = s.id AND m.role = 'user' AND m.content IS NOT NULL
                     ORDER BY m.timestamp, m.id LIMIT 1),
                    ''
                ) AS _preview_raw,
                COALESCE(
                    (SELECT MAX(m2.timestamp) FROM messages m2 WHERE m2.session_id = s.id),
                    s.started_at
                ) AS last_active
             FROM sessions s
             WHERE s.id = ?1",
        )
        .map_err(|e| AgentError::Io(format!("get_session_rich_row: {e}")))?;
    match stmt.query_row(params![session_id], |row| {
        let mut rec = row_to_session(row)?;
        let raw = rec.preview.take().unwrap_or_default();
        rec.preview = format_preview(&raw);
        Ok(rec)
    }) {
        Ok(row) => Ok(Some(row)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(AgentError::Io(format!("get_session_rich_row: {e}"))),
    }
}

pub fn list_sessions_rich(
    conn: &Arc<std::sync::Mutex<Connection>>,
    source: Option<&str>,
    exclude_sources: &[&str],
    limit: usize,
    offset: usize,
    min_message_count: i64,
    include_children: bool,
    project_compression_tips: bool,
    order_by_last_active: bool,
) -> Result<Vec<SessionRecord>, AgentError> {
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;

    let mut where_clauses: Vec<String> = Vec::new();
    if !include_children {
        where_clauses.push(
            "(s.parent_session_id IS NULL OR EXISTS (
                SELECT 1 FROM sessions p
                WHERE p.id = s.parent_session_id
                  AND p.end_reason = 'branched'
                  AND s.started_at >= p.ended_at
            ))"
                .to_string(),
        );
    }
    let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();

    if let Some(src) = source {
        where_clauses.push(
            "COALESCE(NULLIF(s.source, ''), NULLIF(s.platform, ''), 'cli') = ?".into(),
        );
        params_vec.push(src.to_string().into());
    }
    if !exclude_sources.is_empty() {
        let ph: Vec<_> = exclude_sources.iter().map(|_| "?").collect();
        where_clauses.push(format!(
            "COALESCE(NULLIF(s.source, ''), NULLIF(s.platform, ''), 'cli') NOT IN ({})",
            ph.join(", ")
        ));
        for s in exclude_sources {
            params_vec.push((*s).to_string().into());
        }
    }
    if min_message_count > 0 {
        where_clauses.push("s.message_count >= ?".into());
        params_vec.push(min_message_count.into());
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    let rows = if order_by_last_active {
        let mut all_params = params_vec.clone();
        all_params.extend(params_vec.iter().cloned());
        all_params.push((limit as i64).into());
        all_params.push((offset as i64).into());
        let sql = format!(
            "WITH RECURSIVE chain(root_id, cur_id) AS (
                SELECT s.id, s.id FROM sessions s {where_sql}
                UNION ALL
                SELECT c.root_id, child.id
                FROM chain c
                JOIN sessions parent ON parent.id = c.cur_id
                JOIN sessions child ON child.parent_session_id = c.cur_id
                WHERE parent.end_reason = 'compression'
                  AND child.started_at >= parent.ended_at
            ),
            chain_max AS (
                SELECT root_id,
                    MAX(COALESCE(
                        (SELECT MAX(m.timestamp) FROM messages m WHERE m.session_id = cur_id),
                        (SELECT started_at FROM sessions ss WHERE ss.id = cur_id)
                    )) AS effective_last_active
                FROM chain
                GROUP BY root_id
            )
            SELECT s.*,
                COALESCE(
                    (SELECT SUBSTR(REPLACE(REPLACE(m.content, char(10), ' '), char(13), ' '), 1, 63)
                     FROM messages m
                     WHERE m.session_id = s.id AND m.role = 'user' AND m.content IS NOT NULL
                     ORDER BY m.timestamp, m.id LIMIT 1),
                    ''
                ) AS _preview_raw,
                COALESCE(
                    (SELECT MAX(m2.timestamp) FROM messages m2 WHERE m2.session_id = s.id),
                    s.started_at
                ) AS last_active
            FROM sessions s
            LEFT JOIN chain_max cm ON cm.root_id = s.id
            {where_sql}
            ORDER BY COALESCE(cm.effective_last_active, s.started_at) DESC,
                     s.started_at DESC, s.id DESC
            LIMIT ? OFFSET ?"
        );
        let mut stmt = guard
            .prepare(&sql)
            .map_err(|e| AgentError::Io(format!("list_sessions_rich: {e}")))?;
        stmt.query_map(rusqlite::params_from_iter(all_params.iter()), |row| {
            let mut rec = row_to_session(row)?;
            let raw = rec.preview.take().unwrap_or_default();
            rec.preview = format_preview(&raw);
            Ok(rec)
        })
        .map_err(|e| AgentError::Io(format!("list_sessions_rich query: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("list_sessions_rich read: {e}")))?
    } else {
        let sql = format!(
            "SELECT s.*,
                COALESCE(
                    (SELECT SUBSTR(REPLACE(REPLACE(m.content, char(10), ' '), char(13), ' '), 1, 63)
                     FROM messages m
                     WHERE m.session_id = s.id AND m.role = 'user' AND m.content IS NOT NULL
                     ORDER BY m.timestamp, m.id LIMIT 1),
                    ''
                ) AS _preview_raw,
                COALESCE(
                    (SELECT MAX(m2.timestamp) FROM messages m2 WHERE m2.session_id = s.id),
                    s.started_at
                ) AS last_active
             FROM sessions s
             {where_sql}
             ORDER BY s.started_at DESC
             LIMIT ? OFFSET ?"
        );
        params_vec.push((limit as i64).into());
        params_vec.push((offset as i64).into());
        let mut stmt = guard
            .prepare(&sql)
            .map_err(|e| AgentError::Io(format!("list_sessions_rich: {e}")))?;
        stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
            let mut rec = row_to_session(row)?;
            let raw = rec.preview.take().unwrap_or_default();
            rec.preview = format_preview(&raw);
            Ok(rec)
        })
        .map_err(|e| AgentError::Io(format!("list_sessions_rich query: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("list_sessions_rich read: {e}")))?
    };
    drop(guard);
    project_compression_tips_if_needed(conn, rows, include_children, project_compression_tips)
}

fn project_compression_tips_if_needed(
    conn: &Arc<std::sync::Mutex<Connection>>,
    sessions: Vec<SessionRecord>,
    include_children: bool,
    project_compression_tips: bool,
) -> Result<Vec<SessionRecord>, AgentError> {
    if !project_compression_tips || include_children {
        return Ok(sessions);
    }
    let mut projected = Vec::with_capacity(sessions.len());
    for s in sessions {
        if s.end_reason.as_deref() != Some("compression") {
            projected.push(s);
            continue;
        }
        let tip_id = get_compression_tip(conn, &s.id)?;
        if tip_id == s.id {
            projected.push(s);
            continue;
        }
        let Some(tip_row) = get_session_rich_row(conn, &tip_id)? else {
            projected.push(s);
            continue;
        };
        let root_id = s.id.clone();
        let started_at = s.started_at;
        let mut merged = s;
        merged.id = tip_row.id;
        merged.ended_at = tip_row.ended_at;
        merged.end_reason = tip_row.end_reason;
        merged.message_count = tip_row.message_count;
        merged.title = tip_row.title;
        merged.last_active = tip_row.last_active;
        merged.preview = tip_row.preview;
        merged.model = tip_row.model;
        merged.system_prompt = tip_row.system_prompt;
        merged.started_at = started_at;
        merged.lineage_root_id = Some(root_id);
        projected.push(merged);
    }
    Ok(projected)
}

/// Stored message row with database id (for anchored history views).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredMessageRow {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<serde_json::Value>,
    pub tool_name: Option<String>,
    pub reasoning_content: Option<String>,
    pub timestamp: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MessagesAroundResult {
    pub window: Vec<StoredMessageRow>,
    pub messages_before: i64,
    pub messages_after: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AnchoredViewResult {
    pub window: Vec<StoredMessageRow>,
    pub messages_before: i64,
    pub messages_after: i64,
    pub bookend_start: Vec<StoredMessageRow>,
    pub bookend_end: Vec<StoredMessageRow>,
}

fn row_to_stored_message(row: &Row<'_>) -> rusqlite::Result<StoredMessageRow> {
    let tool_calls_json: Option<String> = row.get("tool_calls").ok();
    let tool_calls = tool_calls_json.and_then(|json| serde_json::from_str(&json).ok());
    Ok(StoredMessageRow {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        role: row.get("role")?,
        content: row.get("content").ok(),
        tool_call_id: row.get("tool_call_id").ok(),
        tool_calls,
        tool_name: row.get("tool_name").ok(),
        reasoning_content: row.get("reasoning_content").ok(),
        timestamp: row.get("timestamp")?,
    })
}

pub fn get_messages_around(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    around_message_id: i64,
    window: i64,
) -> Result<MessagesAroundResult, AgentError> {
    let window = window.max(0);
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;

    let anchor_exists: bool = guard
        .query_row(
            "SELECT 1 FROM messages WHERE id = ?1 AND session_id = ?2 LIMIT 1",
            params![around_message_id, session_id],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !anchor_exists {
        return Ok(MessagesAroundResult::default());
    }

    let mut before_stmt = guard
        .prepare(
            "SELECT * FROM messages
             WHERE session_id = ?1 AND id <= ?2
             ORDER BY id DESC LIMIT ?3",
        )
        .map_err(|e| AgentError::Io(format!("get_messages_around before: {e}")))?;
    let before_rows: Vec<StoredMessageRow> = before_stmt
        .query_map(params![session_id, around_message_id, window + 1], row_to_stored_message)
        .map_err(|e| AgentError::Io(format!("get_messages_around before query: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("get_messages_around before read: {e}")))?;

    let mut after_stmt = guard
        .prepare(
            "SELECT * FROM messages
             WHERE session_id = ?1 AND id > ?2
             ORDER BY id ASC LIMIT ?3",
        )
        .map_err(|e| AgentError::Io(format!("get_messages_around after: {e}")))?;
    let after_rows: Vec<StoredMessageRow> = after_stmt
        .query_map(params![session_id, around_message_id, window], row_to_stored_message)
        .map_err(|e| AgentError::Io(format!("get_messages_around after query: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("get_messages_around after read: {e}")))?;

    let messages_before = (before_rows.len() as i64).saturating_sub(1).max(0);
    let messages_after = after_rows.len() as i64;
    let mut window_rows = before_rows;
    window_rows.reverse();
    window_rows.extend(after_rows);

    Ok(MessagesAroundResult {
        window: window_rows,
        messages_before,
        messages_after,
    })
}

pub fn get_anchored_view(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    around_message_id: i64,
    window: i64,
    bookend: i64,
    keep_roles: Option<&[&str]>,
) -> Result<AnchoredViewResult, AgentError> {
    let bookend = bookend.max(0);
    let primitive = get_messages_around(conn, session_id, around_message_id, window)?;
    if primitive.window.is_empty() {
        return Ok(AnchoredViewResult::default());
    }

    let filtered_window: Vec<StoredMessageRow> = if let Some(roles) = keep_roles {
        let keep: std::collections::HashSet<&str> = roles.iter().copied().collect();
        primitive
            .window
            .iter()
            .filter(|m| m.id == around_message_id || keep.contains(m.role.as_str()))
            .cloned()
            .collect()
    } else {
        primitive.window.clone()
    };

    let window_min_id = primitive.window.first().map(|m| m.id).unwrap_or(0);
    let window_max_id = primitive.window.last().map(|m| m.id).unwrap_or(0);

    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;

    let mut bookend_start = Vec::new();
    let mut bookend_end = Vec::new();
    if bookend > 0 {
        let role_list: Vec<String> = keep_roles
            .map(|roles| roles.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let (role_clause, _): (String, Vec<String>) = if !role_list.is_empty() {
            let ph: Vec<_> = role_list.iter().map(|_| "?").collect();
            (format!(" AND role IN ({})", ph.join(", ")), role_list.clone())
        } else {
            (String::new(), Vec::new())
        };

        let start_sql = format!(
            "SELECT * FROM messages
             WHERE session_id = ?1 AND id < ?2{role_clause}
               AND length(content) > 0
             ORDER BY id ASC LIMIT ?"
        );
        let mut start_vals: Vec<rusqlite::types::Value> =
            vec![session_id.to_string().into(), window_min_id.into()];
        start_vals.extend(role_list.iter().cloned().map(rusqlite::types::Value::Text));
        start_vals.push(bookend.into());
        let mut start_stmt = guard
            .prepare(&start_sql)
            .map_err(|e| AgentError::Io(format!("anchored bookend_start: {e}")))?;
        bookend_start = start_stmt
            .query_map(rusqlite::params_from_iter(start_vals.iter()), row_to_stored_message)
            .map_err(|e| AgentError::Io(format!("anchored bookend_start query: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("anchored bookend_start read: {e}")))?;

        let end_sql = format!(
            "SELECT * FROM messages
             WHERE session_id = ?1 AND id > ?2{role_clause}
               AND length(content) > 0
             ORDER BY id DESC LIMIT ?"
        );
        let mut end_vals: Vec<rusqlite::types::Value> =
            vec![session_id.to_string().into(), window_max_id.into()];
        end_vals.extend(role_list.into_iter().map(rusqlite::types::Value::Text));
        end_vals.push(bookend.into());
        let mut end_stmt = guard
            .prepare(&end_sql)
            .map_err(|e| AgentError::Io(format!("anchored bookend_end: {e}")))?;
        bookend_end = end_stmt
            .query_map(rusqlite::params_from_iter(end_vals.iter()), row_to_stored_message)
            .map_err(|e| AgentError::Io(format!("anchored bookend_end query: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("anchored bookend_end read: {e}")))?;
        bookend_end.reverse();
    }

    Ok(AnchoredViewResult {
        window: filtered_window,
        messages_before: primitive.messages_before,
        messages_after: primitive.messages_after,
        bookend_start,
        bookend_end,
    })
}

pub fn row_to_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    let role_str: String = row.get(0)?;
    let content: Option<String> = row.get(1)?;
    let tool_call_id: Option<String> = row.get(2)?;
    let tool_calls_json: Option<String> = row.get(3)?;
    let reasoning_content: Option<String> = row.get(4)?;
    let name: Option<String> = row.get(5).ok();

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
        name,
        reasoning_content,
        cache_control: None,
    })
}

pub fn load_messages(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
) -> Result<Vec<Message>, AgentError> {
    let guard = conn
        .lock()
        .map_err(|_| AgentError::Io("state db lock poisoned".into()))?;
    let mut stmt = guard
        .prepare(
            "SELECT role, content, tool_call_id, tool_calls, reasoning_content, tool_name
             FROM messages
             WHERE session_id = ?1
             ORDER BY timestamp ASC, id ASC",
        )
        .map_err(|e| AgentError::Io(format!("load_messages prepare: {e}")))?;
    stmt.query_map(params![session_id], row_to_message)
        .map_err(|e| AgentError::Io(format!("load_messages query: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Io(format!("load_messages read: {e}")))
}

pub fn append_messages(
    conn: &Arc<std::sync::Mutex<Connection>>,
    session_id: &str,
    messages: &[Message],
) -> Result<(), AgentError> {
    if messages.is_empty() {
        return Ok(());
    }
    let sid = session_id.to_string();
    let owned: Vec<Message> = messages.to_vec();
    execute_write(conn, move |c| {
        let ts = now_unix();
        let insert_sql = hermes_tools::state_db::message_insert_sql(c)
            .map_err(|e| AgentError::Io(e.to_string()))?;
        let mut stmt = c
            .prepare(&insert_sql)
            .map_err(|e| AgentError::Io(format!("append prepare: {e}")))?;
        for msg in &owned {
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
            let tool_name = msg.name.as_deref();
            stmt.execute(params![
                sid,
                role,
                msg.content.as_deref(),
                msg.tool_call_id.as_deref(),
                tool_calls_json.as_deref(),
                tool_name,
                msg.reasoning_content.as_deref(),
                ts,
            ])
            .map_err(|e| AgentError::Io(format!("append insert: {e}")))?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_persistence::schema::init_schema;
    use std::sync::Mutex;

    fn mem_conn() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn title_sanitize_and_set() {
        let conn = mem_conn();
        create_session(&conn, "s1", "cli", None, None, None, None).unwrap();
        set_session_title(&conn, "s1", Some("My Session")).unwrap();
        assert_eq!(
            get_session_title(&conn, "s1").unwrap().as_deref(),
            Some("My Session")
        );
    }

    #[test]
    fn resolve_resume_follows_empty_parent_to_child() {
        let conn = mem_conn();
        create_session(&conn, "parent", "cli", None, None, None, None).unwrap();
        create_session(&conn, "child", "cli", None, Some("parent"), None, None).unwrap();
        append_messages(
            &conn,
            "child",
            &[Message::user("hello")],
        )
        .unwrap();
        assert_eq!(
            resolve_resume_session_id(&conn, "parent").unwrap(),
            "child"
        );
    }
}
