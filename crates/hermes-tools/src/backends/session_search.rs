//! Real session search backend using rusqlite with FTS5.

use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tokio::task::JoinSet;

use crate::tools::session_search::SessionSearchBackend;
use hermes_core::ToolError;

const MAX_SESSION_CHARS: usize = 100_000;
const MAX_SUMMARY_TOKENS: usize = 10_000;
const HIDDEN_SESSION_SOURCES: &[&str] = &["tool", "internal"];

/// Real session search backend using SQLite FTS5.
pub struct SqliteSessionSearchBackend {
    conn: Mutex<Connection>,
    fts_enabled: bool,
}

#[derive(Clone)]
struct SessionSummaryClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Clone)]
struct SummaryTask {
    session_id: String,
    source: String,
    when: Option<String>,
    model: Option<String>,
    conversation_text: String,
}

#[derive(Clone)]
struct SessionRowContext {
    id: String,
    created_at: Option<String>,
    parent_session_id: Option<String>,
    model_config: Option<String>,
    parent_end_reason: Option<String>,
    parent_ended_at: Option<String>,
    updated_at: Option<String>,
}

impl SqliteSessionSearchBackend {
    fn is_fts5_unavailable_message(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("no such module")
            || lower.contains("unknown tokenizer")
            || lower.contains("fts5")
    }

    fn is_fts5_unavailable_error(error: &rusqlite::Error) -> bool {
        Self::is_fts5_unavailable_message(&error.to_string())
    }

    fn fts_table_exists(conn: &Connection) -> Result<bool, ToolError> {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages_fts' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            other => Err(ToolError::ExecutionFailed(format!(
                "Failed to inspect messages_fts availability: {other}"
            ))),
        })
    }

    fn ensure_fts_schema(conn: &Connection, db_path: &str) -> Result<bool, ToolError> {
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
                    "SQLite FTS5 unavailable for {}; session_search will use recent/id lookup only: {}",
                    db_path,
                    err
                );
                Ok(false)
            }
            Err(err) => Err(ToolError::ExecutionFailed(format!(
                "Failed to ensure session FTS schema: {err}"
            ))),
        }
    }

    fn ensure_text_column(conn: &Connection, table: &str, column: &str) -> Result<(), ToolError> {
        match conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"),
            rusqlite::params![],
        ) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column name") {
                    Ok(())
                } else {
                    Err(ToolError::ExecutionFailed(format!(
                        "Failed to ensure {table}.{column}: {e}"
                    )))
                }
            }
        }
    }

    fn ensure_session_compat_columns(conn: &Connection) -> Result<(), ToolError> {
        for column in [
            "parent_session_id",
            "model_config",
            "end_reason",
            "ended_at",
        ] {
            Self::ensure_text_column(conn, "sessions", column)?;
        }
        Ok(())
    }

    fn branch_marker_from_model_config(model_config: Option<&str>) -> Option<String> {
        let raw = model_config?.trim();
        if raw.is_empty() {
            return None;
        }
        serde_json::from_str::<Value>(raw).ok().and_then(|v| {
            v.get("_branched_from")
                .and_then(|marker| marker.as_str())
                .map(str::trim)
                .filter(|marker| !marker.is_empty())
                .map(ToString::to_string)
        })
    }

    fn parse_timestamp_utc(raw: Option<&str>) -> Option<DateTime<Utc>> {
        let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
        if let Ok(seconds) = raw.parse::<f64>() {
            let sec = seconds.trunc() as i64;
            let nanos = ((seconds.fract().abs()) * 1_000_000_000_f64).round() as u32;
            if let Some(dt) = Utc.timestamp_opt(sec, nanos).single() {
                return Some(dt);
            }
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
            return Some(Utc.from_utc_datetime(&naive));
        }
        None
    }

    fn is_legacy_branch_child(
        created_at: Option<&str>,
        parent_end_reason: Option<&str>,
        parent_ended_at: Option<&str>,
    ) -> bool {
        if parent_end_reason.map(str::trim) != Some("branched") {
            return false;
        }
        match (
            Self::parse_timestamp_utc(created_at),
            Self::parse_timestamp_utc(parent_ended_at),
        ) {
            (Some(child_started), Some(parent_ended)) => child_started >= parent_ended,
            // If an old DB has the reason but not a parseable ended_at, keep the
            // branch visible rather than hiding potentially user-created work.
            _ => parent_ended_at
                .map(str::trim)
                .unwrap_or_default()
                .is_empty(),
        }
    }

    fn is_branch_child(
        parent_session_id: Option<&str>,
        model_config: Option<&str>,
        created_at: Option<&str>,
        parent_end_reason: Option<&str>,
        parent_ended_at: Option<&str>,
    ) -> bool {
        let has_parent = parent_session_id
            .map(str::trim)
            .filter(|parent| !parent.is_empty())
            .is_some();
        has_parent
            && (Self::branch_marker_from_model_config(model_config).is_some()
                || Self::is_legacy_branch_child(created_at, parent_end_reason, parent_ended_at))
    }

    fn is_compression_child(
        created_at: Option<&str>,
        parent_end_reason: Option<&str>,
        parent_ended_at: Option<&str>,
    ) -> bool {
        if parent_end_reason.map(str::trim) != Some("compression") {
            return false;
        }
        match (
            Self::parse_timestamp_utc(created_at),
            Self::parse_timestamp_utc(parent_ended_at),
        ) {
            (Some(child_started), Some(parent_ended)) => child_started >= parent_ended,
            _ => false,
        }
    }

    fn session_row_context(conn: &Connection, session_id: &str) -> Option<SessionRowContext> {
        conn.query_row(
            "SELECT s.id, s.created_at, s.parent_session_id, s.model_config,
                    p.end_reason, p.ended_at, s.updated_at
             FROM sessions s
             LEFT JOIN sessions p ON p.id = s.parent_session_id
             WHERE s.id = ?1",
            rusqlite::params![session_id],
            |row| {
                Ok(SessionRowContext {
                    id: row.get::<_, String>(0)?,
                    created_at: row.get::<_, Option<String>>(1)?,
                    parent_session_id: row.get::<_, Option<String>>(2)?,
                    model_config: row.get::<_, Option<String>>(3)?,
                    parent_end_reason: row.get::<_, Option<String>>(4)?,
                    parent_ended_at: row.get::<_, Option<String>>(5)?,
                    updated_at: row.get::<_, Option<String>>(6)?,
                })
            },
        )
        .ok()
    }

    fn compression_root(conn: &Connection, session_id: &str) -> String {
        let mut visited = HashSet::new();
        let mut sid = session_id.to_string();
        loop {
            if sid.is_empty() || !visited.insert(sid.clone()) {
                break;
            }
            let Some(ctx) = Self::session_row_context(conn, &sid) else {
                break;
            };
            if !Self::is_compression_child(
                ctx.created_at.as_deref(),
                ctx.parent_end_reason.as_deref(),
                ctx.parent_ended_at.as_deref(),
            ) {
                break;
            }
            let Some(parent) = ctx
                .parent_session_id
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
            else {
                break;
            };
            sid = parent.to_string();
        }
        sid
    }

    fn compression_tip(conn: &Connection, root_session_id: &str) -> String {
        let mut visited = HashSet::new();
        let mut sid = root_session_id.to_string();
        loop {
            if sid.is_empty() || !visited.insert(sid.clone()) {
                break;
            }
            let child = conn
                .query_row(
                    "SELECT c.id
                     FROM sessions c
                     JOIN sessions p ON p.id = c.parent_session_id
                     WHERE c.parent_session_id = ?1
                       AND p.end_reason = 'compression'
                       AND c.created_at >= p.ended_at
                     ORDER BY c.created_at DESC, c.updated_at DESC, c.id DESC
                     LIMIT 1",
                    rusqlite::params![sid.clone()],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty());
            match child {
                Some(next) => sid = next,
                None => break,
            }
        }
        sid
    }

    fn logical_session_key_for_row(
        conn: &Connection,
        session_id: &str,
        parent_session_id: Option<&str>,
        model_config: Option<&str>,
        created_at: Option<&str>,
        parent_end_reason: Option<&str>,
        parent_ended_at: Option<&str>,
    ) -> String {
        if Self::is_branch_child(
            parent_session_id,
            model_config,
            created_at,
            parent_end_reason,
            parent_ended_at,
        ) {
            return session_id.to_string();
        }
        if Self::is_compression_child(created_at, parent_end_reason, parent_ended_at) {
            return Self::compression_root(conn, session_id);
        }
        Self::resolve_to_parent(conn, session_id)
    }

    fn visible_session_id_for_row(
        conn: &Connection,
        session_id: &str,
        parent_session_id: Option<&str>,
        model_config: Option<&str>,
        created_at: Option<&str>,
        parent_end_reason: Option<&str>,
        parent_ended_at: Option<&str>,
    ) -> String {
        if Self::is_branch_child(
            parent_session_id,
            model_config,
            created_at,
            parent_end_reason,
            parent_ended_at,
        ) {
            session_id.to_string()
        } else {
            let root = if Self::is_compression_child(created_at, parent_end_reason, parent_ended_at)
            {
                Self::compression_root(conn, session_id)
            } else {
                Self::resolve_to_parent(conn, session_id)
            };
            Self::compression_tip(conn, &root)
        }
    }

    fn logical_session_key_for_session_id(conn: &Connection, session_id: &str) -> String {
        Self::session_row_context(conn, session_id)
            .map(|ctx| {
                Self::logical_session_key_for_row(
                    conn,
                    &ctx.id,
                    ctx.parent_session_id.as_deref(),
                    ctx.model_config.as_deref(),
                    ctx.created_at.as_deref(),
                    ctx.parent_end_reason.as_deref(),
                    ctx.parent_ended_at.as_deref(),
                )
            })
            .unwrap_or_else(|| Self::resolve_to_parent(conn, session_id))
    }

    fn escape_like(raw: &str) -> String {
        raw.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    }

    fn id_match_score(needle: &str, values: &[&str]) -> u8 {
        if values
            .iter()
            .any(|value| value.eq_ignore_ascii_case(needle))
        {
            0
        } else if values
            .iter()
            .any(|value| value.to_lowercase().starts_with(needle))
        {
            1
        } else {
            2
        }
    }

    fn search_sessions_by_id(
        conn: &Connection,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionRowContext>, ToolError> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let hidden_placeholders = HIDDEN_SESSION_SOURCES
            .iter()
            .map(|_| "?".to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT s.id, s.created_at, s.parent_session_id, s.model_config,
                    p.end_reason, p.ended_at, s.updated_at
             FROM sessions s
             LEFT JOIN sessions p ON p.id = s.parent_session_id
             WHERE LOWER(s.id) LIKE ? ESCAPE '\\'
               AND COALESCE(s.platform, '') NOT IN ({})
             ORDER BY s.updated_at DESC, s.created_at DESC, s.id DESC
             LIMIT ?",
            hidden_placeholders
        );
        let mut values: Vec<rusqlite::types::Value> = vec![rusqlite::types::Value::Text(format!(
            "%{}%",
            Self::escape_like(&needle)
        ))];
        values.extend(
            HIDDEN_SESSION_SOURCES
                .iter()
                .map(|s| rusqlite::types::Value::Text((*s).to_string())),
        );
        values.push(rusqlite::types::Value::Integer(
            (limit * 4).max(limit) as i64
        ));
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to prepare session id search query: {e}"))
        })?;
        let rows = stmt
            .query_map(params, |row| {
                Ok(SessionRowContext {
                    id: row.get::<_, String>(0)?,
                    created_at: row.get::<_, Option<String>>(1)?,
                    parent_session_id: row.get::<_, Option<String>>(2)?,
                    model_config: row.get::<_, Option<String>>(3)?,
                    parent_end_reason: row.get::<_, Option<String>>(4)?,
                    parent_ended_at: row.get::<_, Option<String>>(5)?,
                    updated_at: row.get::<_, Option<String>>(6)?,
                })
            })
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Session id search query failed: {e}"))
            })?;

        let mut ranked = Vec::new();
        for (idx, row) in rows.flatten().enumerate() {
            let logical_key = Self::logical_session_key_for_row(
                conn,
                &row.id,
                row.parent_session_id.as_deref(),
                row.model_config.as_deref(),
                row.created_at.as_deref(),
                row.parent_end_reason.as_deref(),
                row.parent_ended_at.as_deref(),
            );
            let score = Self::id_match_score(&needle, &[row.id.as_str(), logical_key.as_str()]);
            ranked.push((score, idx, row));
        }
        ranked.sort_by_key(|(score, idx, row)| (*score, *idx, row.updated_at.clone()));
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, row)| row)
            .collect())
    }

    fn session_metadata(
        conn: &Connection,
        session_id: &str,
    ) -> (Option<String>, String, Option<String>) {
        conn.query_row(
            "SELECT created_at, COALESCE(platform, 'cli'), model FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?
                        .unwrap_or_else(|| "cli".to_string()),
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap_or_else(|_| (None, "cli".to_string(), None))
    }

    fn load_summary_task(
        conn: &Connection,
        session_id: &str,
        query: &str,
        allow_empty_id_hit: bool,
    ) -> Result<Option<SummaryTask>, ToolError> {
        let (started_at, source, model) = Self::session_metadata(conn, session_id);
        let mut msg_stmt = conn
            .prepare(
                "SELECT role, COALESCE(content, ''), tool_calls
                 FROM messages WHERE session_id = ?1 ORDER BY id ASC",
            )
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to prepare messages query: {e}"))
            })?;
        let msg_rows = msg_stmt
            .query_map(rusqlite::params![session_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to load session messages: {e}"))
            })?;
        let messages: Vec<(String, String, Option<String>)> = msg_rows.flatten().collect();
        let conversation_text = if messages.is_empty() {
            if !allow_empty_id_hit {
                return Ok(None);
            }
            format!("Session ID: {session_id}")
        } else {
            let transcript = Self::format_conversation(&messages);
            Self::truncate_around_matches(&transcript, query, MAX_SESSION_CHARS)
        };
        Ok(Some(SummaryTask {
            session_id: session_id.to_string(),
            source,
            when: Some(Self::format_timestamp(started_at.as_deref())),
            model,
            conversation_text,
        }))
    }

    fn resolve_to_parent(conn: &Connection, session_id: &str) -> String {
        let mut visited = HashSet::new();
        let mut sid = session_id.to_string();
        loop {
            if sid.is_empty() || !visited.insert(sid.clone()) {
                break;
            }
            let parent = conn
                .query_row(
                    "SELECT parent_session_id FROM sessions WHERE id = ?1",
                    rusqlite::params![sid.clone()],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty());
            match parent {
                Some(next) => sid = next,
                None => break,
            }
        }
        sid
    }

    fn format_conversation(messages: &[(String, String, Option<String>)]) -> String {
        let mut parts = Vec::new();
        for (role_raw, content_raw, tool_calls_raw) in messages {
            let role_upper = role_raw.to_uppercase();
            let mut content = content_raw.clone();

            if role_upper == "TOOL" {
                if content.chars().count() > 500 {
                    let head: String = content.chars().take(250).collect();
                    let tail: String = content
                        .chars()
                        .rev()
                        .take(250)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    content = format!("{head}\n...[truncated]...\n{tail}");
                }
                parts.push(format!("[TOOL]: {content}"));
                continue;
            }

            if role_upper == "ASSISTANT" {
                if let Some(raw) = tool_calls_raw {
                    let mut names = Vec::new();
                    if let Ok(v) = serde_json::from_str::<Value>(raw) {
                        if let Some(arr) = v.as_array() {
                            for tc in arr {
                                let name = tc
                                    .get("name")
                                    .and_then(|x| x.as_str())
                                    .or_else(|| {
                                        tc.get("function")
                                            .and_then(|f| f.get("name"))
                                            .and_then(|x| x.as_str())
                                    })
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty());
                                if let Some(n) = name {
                                    names.push(n.to_string());
                                }
                            }
                        }
                    }
                    if !names.is_empty() {
                        parts.push(format!("[ASSISTANT]: [Called: {}]", names.join(", ")));
                    }
                }
                if !content.trim().is_empty() {
                    parts.push(format!("[ASSISTANT]: {content}"));
                }
                continue;
            }

            parts.push(format!("[{role_upper}]: {content}"));
        }
        parts.join("\n\n")
    }

    fn truncate_around_matches(full_text: &str, query: &str, max_chars: usize) -> String {
        if full_text.chars().count() <= max_chars {
            return full_text.to_string();
        }
        let text_lower = full_text.to_lowercase();
        let mut first_match = text_lower.len();
        for term in query.to_lowercase().split_whitespace() {
            let t = term.trim();
            if t.is_empty() {
                continue;
            }
            if let Some(pos) = text_lower.find(t) {
                first_match = first_match.min(pos);
            }
        }
        if first_match == text_lower.len() {
            first_match = 0;
        }

        let half = max_chars / 2;
        let mut start = first_match.saturating_sub(half);
        let end = (start + max_chars).min(full_text.len());
        if end.saturating_sub(start) < max_chars {
            start = end.saturating_sub(max_chars);
        }
        let body = &full_text[start..end];
        let prefix = if start > 0 {
            "...[earlier conversation truncated]...\n\n"
        } else {
            ""
        };
        let suffix = if end < full_text.len() {
            "\n\n...[later conversation truncated]..."
        } else {
            ""
        };
        format!("{prefix}{body}{suffix}")
    }

    fn format_timestamp(ts: Option<&str>) -> String {
        let Some(raw) = ts.map(str::trim).filter(|s| !s.is_empty()) else {
            return "unknown".to_string();
        };

        let format_human =
            |dt: DateTime<Local>| -> String { dt.format("%B %d, %Y at %I:%M %p").to_string() };

        if let Ok(seconds) = raw.parse::<f64>() {
            let sec = seconds.trunc() as i64;
            let nanos = ((seconds.fract().abs()) * 1_000_000_000_f64).round() as u32;
            if let Some(dt_utc) = Utc.timestamp_opt(sec, nanos).single() {
                return format_human(dt_utc.with_timezone(&Local));
            }
        }

        if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
            return format_human(dt.with_timezone(&Local));
        }

        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
            if let Some(local) = Local.from_local_datetime(&naive).single() {
                return format_human(local);
            }
        }

        raw.to_string()
    }

    fn summary_client_from_env() -> Option<SessionSummaryClient> {
        let model = std::env::var("HERMES_SESSION_SEARCH_SUMMARY_MODEL")
            .ok()
            .or_else(|| std::env::var("HERMES_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o-mini".to_string());
        let model = model
            .split(':')
            .next_back()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("gpt-4o-mini")
            .to_string();

        let base_url = std::env::var("HERMES_SESSION_SEARCH_SUMMARY_BASE_URL")
            .ok()
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
            .trim()
            .trim_end_matches('/')
            .to_string();

        let mut api_key = std::env::var("HERMES_SESSION_SEARCH_SUMMARY_API_KEY")
            .ok()
            .or_else(|| std::env::var("HERMES_OPENAI_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_default();
        if api_key.trim().is_empty() && base_url.to_lowercase().contains("openrouter.ai") {
            api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
        }
        if api_key.trim().is_empty() {
            return None;
        }

        Some(SessionSummaryClient {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
        })
    }

    async fn summarize_one(
        summary_client: &SessionSummaryClient,
        query: &str,
        task: &SummaryTask,
    ) -> Option<String> {
        let system_prompt = "You are reviewing a past conversation transcript to help recall what happened. Summarize the conversation with a focus on the search topic. Include: 1) user goal, 2) actions and outcomes, 3) key decisions/solutions, 4) specific commands/files/URLs/errors, 5) unresolved items. Be thorough but concise and factual in past tense.";
        let user_prompt = format!(
            "Search topic: {query}\nSession source: {}\nSession date: {}\n\nCONVERSATION TRANSCRIPT:\n{}\n\nSummarize this conversation with focus on: {query}",
            task.source,
            task.when.clone().unwrap_or_else(|| "unknown".to_string()),
            task.conversation_text,
        );

        let url = format!("{}/chat/completions", summary_client.base_url);
        for attempt in 0..3 {
            let request_body = json!({
                "model": summary_client.model,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt},
                ],
                "temperature": 0.1,
                "max_tokens": MAX_SUMMARY_TOKENS,
            });
            let mut req = summary_client
                .client
                .post(&url)
                .bearer_auth(summary_client.api_key.trim())
                .timeout(Duration::from_secs(60))
                .json(&request_body);
            if summary_client
                .base_url
                .to_lowercase()
                .contains("openrouter.ai")
            {
                req = req
                    .header("HTTP-Referer", "https://hermes-agent.nousresearch.com")
                    .header("X-OpenRouter-Title", "Hermes Agent");
            }
            let resp = req.send().await;
            if let Ok(ok_resp) = resp {
                if let Ok(v) = ok_resp.json::<Value>().await {
                    let text = v
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|x| x.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .map(str::trim)
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_secs((attempt + 1) as u64)).await;
            }
        }
        None
    }

    /// Open or create the sessions database at the given path.
    pub fn new(db_path: &str) -> Result<Self, ToolError> {
        if let Some(parent) = std::path::Path::new(db_path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to create session db directory: {}", e))
            })?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to open sessions DB: {}", e))
        })?;

        // Align with session persistence schema used by the agent loop.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                title TEXT,
                message_count INTEGER DEFAULT 0,
                parent_session_id TEXT,
                model_config TEXT,
                end_reason TEXT,
                ended_at TEXT
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);",
        )
        .map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to ensure session schema: {}", e))
        })?;

        Self::ensure_session_compat_columns(&conn)?;
        let fts_enabled = Self::ensure_fts_schema(&conn, db_path)?;

        Ok(Self {
            conn: Mutex::new(conn),
            fts_enabled,
        })
    }

    /// Open or create the default sessions database at ~/.hermes/sessions.db.
    pub fn default_path() -> Result<Self, ToolError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let hermes_dir = std::path::Path::new(&home).join(".hermes");
        std::fs::create_dir_all(&hermes_dir).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to create ~/.hermes: {}", e))
        })?;
        let db_path = hermes_dir.join("sessions.db");
        Self::new(db_path.to_str().unwrap_or("sessions.db"))
    }

    /// Index a message into the FTS5 table.
    pub fn index_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        timestamp: &str,
    ) -> Result<(), ToolError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, role, content, timestamp],
        )
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to index message: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl SessionSearchBackend for SqliteSessionSearchBackend {
    async fn search(
        &self,
        query: Option<&str>,
        role_filter: Option<&str>,
        limit: usize,
        current_session_id: Option<&str>,
    ) -> Result<String, ToolError> {
        let query = query.map(str::trim).unwrap_or("");
        let limit = limit.min(5).max(1);

        let (tasks, sessions_searched, recent_payload, search_degraded): (
            Vec<SummaryTask>,
            usize,
            Option<Value>,
            Option<&'static str>,
        ) = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

            // Recent-mode parity: no query means list recent sessions metadata.
            if query.is_empty() {
                let placeholders = HIDDEN_SESSION_SOURCES
                    .iter()
                    .map(|_| "?")
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT s.id, s.title, s.platform, s.created_at, s.updated_at,
                            s.message_count, s.parent_session_id, s.model_config,
                            p.end_reason, p.ended_at
                     FROM sessions s
                     LEFT JOIN sessions p ON p.id = s.parent_session_id
                     WHERE COALESCE(s.platform, '') NOT IN ({})
                     ORDER BY s.updated_at DESC
                     LIMIT ?",
                    placeholders
                );
                let mut stmt = conn.prepare(&sql).map_err(|e| {
                    ToolError::ExecutionFailed(format!(
                        "Failed to prepare recent sessions query: {}",
                        e
                    ))
                })?;
                let mut params_values: Vec<rusqlite::types::Value> = HIDDEN_SESSION_SOURCES
                    .iter()
                    .map(|s| rusqlite::types::Value::Text((*s).to_string()))
                    .collect();
                let scan_limit = (limit.saturating_mul(5).saturating_add(20)).max(limit);
                params_values.push(rusqlite::types::Value::Integer(scan_limit as i64));
                let params = rusqlite::params_from_iter(params_values.iter());
                let rows = stmt
                    .query_map(params, |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?
                                .unwrap_or_else(|| "cli".to_string()),
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5).unwrap_or(0),
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, Option<String>>(8)?,
                            row.get::<_, Option<String>>(9)?,
                        ))
                    })
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("Recent sessions query failed: {}", e))
                    })?;

                let current_lineage_root = current_session_id
                    .map(|sid| Self::logical_session_key_for_session_id(&conn, sid.trim()));
                let mut results = Vec::new();
                for row in rows.flatten() {
                    let (
                        session_id,
                        title,
                        source,
                        started_at,
                        last_active,
                        message_count,
                        parent_session_id,
                        model_config,
                        parent_end_reason,
                        parent_ended_at,
                    ) = row;
                    let has_parent = parent_session_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|parent| !parent.is_empty())
                        .is_some();
                    if has_parent
                        && !Self::is_branch_child(
                            parent_session_id.as_deref(),
                            model_config.as_deref(),
                            Some(&started_at),
                            parent_end_reason.as_deref(),
                            parent_ended_at.as_deref(),
                        )
                    {
                        continue;
                    }
                    if let Some(ref current_root) = current_lineage_root {
                        if current_root == &session_id {
                            continue;
                        }
                    }
                    let preview: String = conn
                        .query_row(
                            "SELECT COALESCE(content, '') FROM messages
                             WHERE session_id = ?1 AND content IS NOT NULL
                             ORDER BY id DESC LIMIT 1",
                            rusqlite::params![session_id.clone()],
                            |r| r.get::<_, String>(0),
                        )
                        .unwrap_or_default();
                    let preview = if preview.chars().count() > 200 {
                        format!("{}…", preview.chars().take(200).collect::<String>())
                    } else {
                        preview
                    };
                    results.push(json!({
                        "session_id": session_id,
                        "title": title,
                        "source": source,
                        "started_at": started_at,
                        "last_active": last_active,
                        "message_count": message_count,
                        "preview": preview,
                    }));
                    if results.len() >= limit {
                        break;
                    }
                }
                let payload = json!({
                    "success": true,
                    "mode": "recent",
                    "results": results,
                    "count": results.len(),
                    "message": format!(
                        "Showing {} most recent sessions. Use a keyword query to search specific topics.",
                        results.len()
                    ),
                });
                (Vec::new(), 0, Some(payload), None)
            } else {
                let mut seen = HashSet::new();
                let current_lineage_root = current_session_id
                    .map(|sid| Self::logical_session_key_for_session_id(&conn, sid.trim()));
                let mut tasks = Vec::new();

                for row in Self::search_sessions_by_id(&conn, query, limit)? {
                    let visible_session_id = Self::visible_session_id_for_row(
                        &conn,
                        &row.id,
                        row.parent_session_id.as_deref(),
                        row.model_config.as_deref(),
                        row.created_at.as_deref(),
                        row.parent_end_reason.as_deref(),
                        row.parent_ended_at.as_deref(),
                    );
                    let logical_key = Self::logical_session_key_for_row(
                        &conn,
                        &row.id,
                        row.parent_session_id.as_deref(),
                        row.model_config.as_deref(),
                        row.created_at.as_deref(),
                        row.parent_end_reason.as_deref(),
                        row.parent_ended_at.as_deref(),
                    );
                    if current_lineage_root.as_ref() == Some(&logical_key) {
                        continue;
                    }
                    if !seen.insert(logical_key) {
                        continue;
                    }
                    if let Some(task) =
                        Self::load_summary_task(&conn, &visible_session_id, query, true)?
                    {
                        tasks.push(task);
                    }
                    if tasks.len() >= limit {
                        break;
                    }
                }

                let fts_available = self.fts_enabled && Self::fts_table_exists(&conn)?;
                let mut search_degraded = None;
                if tasks.len() < limit && fts_available {
                    let hidden_placeholders = HIDDEN_SESSION_SOURCES
                        .iter()
                        .map(|_| "?".to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mut sql = String::from(
                        "SELECT m.session_id, s.created_at, s.platform, s.model,
                                s.parent_session_id, s.model_config,
                                p.end_reason, p.ended_at,
                                bm25(messages_fts) AS rank
                         FROM messages_fts
                         JOIN messages m ON m.id = messages_fts.rowid
                         LEFT JOIN sessions s ON s.id = m.session_id
                         LEFT JOIN sessions p ON p.id = s.parent_session_id
                         WHERE messages_fts MATCH ?1
                           AND m.content IS NOT NULL
                           AND m.content != ''
                           AND COALESCE(s.platform, '') NOT IN (",
                    );
                    sql.push_str(&hidden_placeholders);
                    sql.push(')');

                    let mut role_values = Vec::new();
                    if let Some(raw_roles) = role_filter {
                        for role in raw_roles
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            role_values.push(role.to_string());
                        }
                        if !role_values.is_empty() {
                            let placeholders = (0..role_values.len())
                                .map(|_| "?".to_string())
                                .collect::<Vec<_>>()
                                .join(", ");
                            sql.push_str(&format!(" AND m.role IN ({})", placeholders));
                        }
                    }
                    sql.push_str(" ORDER BY rank LIMIT 50");

                    let mut stmt = conn.prepare(&sql).map_err(|e| {
                        ToolError::ExecutionFailed(format!(
                            "Failed to prepare session search query: {}",
                            e
                        ))
                    })?;

                    let mut values: Vec<rusqlite::types::Value> =
                        vec![rusqlite::types::Value::Text(query.to_string())];
                    values.extend(
                        HIDDEN_SESSION_SOURCES
                            .iter()
                            .map(|s| rusqlite::types::Value::Text((*s).to_string())),
                    );
                    values.extend(role_values.into_iter().map(rusqlite::types::Value::Text));
                    let params = rusqlite::params_from_iter(values.iter());

                    let rows = stmt
                        .query_map(params, |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, Option<String>>(1)?,
                                row.get::<_, Option<String>>(2)?
                                    .unwrap_or_else(|| "cli".to_string()),
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, Option<String>>(5)?,
                                row.get::<_, Option<String>>(6)?,
                                row.get::<_, Option<String>>(7)?,
                            ))
                        })
                        .map_err(|e| {
                            ToolError::ExecutionFailed(format!(
                                "Session search query failed: {}",
                                e
                            ))
                        })?;

                    for row in rows.flatten() {
                        if tasks.len() >= limit {
                            break;
                        }
                        let (
                            raw_session_id,
                            raw_started_at,
                            _raw_source,
                            _raw_model,
                            parent_session_id,
                            model_config,
                            parent_end_reason,
                            parent_ended_at,
                        ) = row;
                        let resolved_session_id = Self::visible_session_id_for_row(
                            &conn,
                            &raw_session_id,
                            parent_session_id.as_deref(),
                            model_config.as_deref(),
                            raw_started_at.as_deref(),
                            parent_end_reason.as_deref(),
                            parent_ended_at.as_deref(),
                        );
                        let logical_key = Self::logical_session_key_for_row(
                            &conn,
                            &raw_session_id,
                            parent_session_id.as_deref(),
                            model_config.as_deref(),
                            raw_started_at.as_deref(),
                            parent_end_reason.as_deref(),
                            parent_ended_at.as_deref(),
                        );
                        if let Some(ref current_root) = current_lineage_root {
                            if &logical_key == current_root {
                                continue;
                            }
                        }
                        if !seen.insert(logical_key) {
                            continue;
                        }
                        if let Some(task) =
                            Self::load_summary_task(&conn, &resolved_session_id, query, false)?
                        {
                            tasks.push(task);
                        }
                    }
                } else if !fts_available {
                    search_degraded = Some("fts5_unavailable");
                }

                let searched = seen.len();
                (tasks, searched, None, search_degraded)
            }
        };

        if let Some(payload) = recent_payload {
            return Ok(payload.to_string());
        }

        let summary_client = Self::summary_client_from_env();
        let mut summaries = Vec::new();
        if let Some(summary_client) = summary_client {
            let mut join_set = JoinSet::new();
            for (idx, task) in tasks.iter().cloned().enumerate() {
                let summary_client = summary_client.clone();
                let q = query.to_string();
                join_set.spawn(async move {
                    let summary = Self::summarize_one(&summary_client, &q, &task).await;
                    (idx, task, summary)
                });
            }
            let mut ordered: Vec<Option<(SummaryTask, Option<String>)>> = vec![None; tasks.len()];
            while let Some(joined) = join_set.join_next().await {
                if let Ok((idx, task, summary)) = joined {
                    if idx < ordered.len() {
                        ordered[idx] = Some((task, summary));
                    }
                }
            }
            for item in ordered.into_iter().flatten() {
                let (task, maybe_summary) = item;
                let summary = maybe_summary.unwrap_or_else(|| {
                    let preview = if task.conversation_text.chars().count() > 500 {
                        format!(
                            "{}\n…[truncated]",
                            task.conversation_text.chars().take(500).collect::<String>()
                        )
                    } else if task.conversation_text.trim().is_empty() {
                        "No preview available.".to_string()
                    } else {
                        task.conversation_text.clone()
                    };
                    format!("[Raw preview — summarization unavailable]\n{preview}")
                });
                summaries.push(json!({
                    "session_id": task.session_id,
                    "when": task.when,
                    "source": task.source,
                    "model": task.model,
                    "summary": summary,
                }));
            }
        } else {
            for task in tasks {
                let preview = if task.conversation_text.chars().count() > 500 {
                    format!(
                        "{}\n…[truncated]",
                        task.conversation_text.chars().take(500).collect::<String>()
                    )
                } else if task.conversation_text.trim().is_empty() {
                    "No preview available.".to_string()
                } else {
                    task.conversation_text.clone()
                };
                summaries.push(json!({
                    "session_id": task.session_id,
                    "when": task.when,
                    "source": task.source,
                    "model": task.model,
                    "summary": format!("[Raw preview — summarization unavailable]\n{}", preview),
                }));
            }
        }

        let mut payload = json!({
            "success": true,
            "query": query,
            "results": summaries,
            "count": summaries.len(),
            "sessions_searched": sessions_searched,
        });
        if let Some(reason) = search_degraded {
            payload["search_degraded"] = json!(reason);
        }
        Ok(payload.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::SqliteSessionSearchBackend;
    use crate::tools::session_search::SessionSearchBackend;
    use rusqlite::Connection;
    use serde_json::Value;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn backend_with_db() -> (TempDir, SqliteSessionSearchBackend) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("sessions.db");
        let backend =
            SqliteSessionSearchBackend::new(db_path.to_str().expect("utf8 path")).expect("backend");
        (tmp, backend)
    }

    fn db_conn(tmp: &TempDir) -> Connection {
        Connection::open(tmp.path().join("sessions.db")).expect("open db")
    }

    fn no_fts_backend_with_seeded_db() -> SqliteSessionSearchBackend {
        let conn = Connection::open_in_memory().expect("open memory db");
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                title TEXT,
                message_count INTEGER DEFAULT 0,
                parent_session_id TEXT,
                model_config TEXT,
                end_reason TEXT,
                ended_at TEXT
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX idx_messages_session ON messages(session_id);",
        )
        .expect("schema");
        insert_session(
            &conn,
            "nofts-session",
            None,
            None,
            None,
            None,
            "2026-01-01T00:00:00Z",
        );
        insert_message(
            &conn,
            "nofts-session",
            "user",
            "uniquephrase only exists in plain messages",
        );
        SqliteSessionSearchBackend {
            conn: Mutex::new(conn),
            fts_enabled: false,
        }
    }

    fn insert_session(
        conn: &Connection,
        id: &str,
        parent: Option<&str>,
        model_config: Option<&str>,
        end_reason: Option<&str>,
        ended_at: Option<&str>,
        created_at: &str,
    ) {
        conn.execute(
            "INSERT INTO sessions (
                id, model, platform, created_at, updated_at, title, message_count,
                parent_session_id, model_config, end_reason, ended_at
             )
             VALUES (?1, 'test/model', 'cli', ?6, ?6, ?1, 0, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, parent, model_config, end_reason, ended_at, created_at],
        )
        .expect("insert session");
    }

    fn insert_message(conn: &Connection, session_id: &str, role: &str, content: &str) {
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at)
             VALUES (?1, ?2, ?3, '2026-01-01T00:00:30Z')",
            rusqlite::params![session_id, role, content],
        )
        .expect("insert message");
        conn.execute(
            "UPDATE sessions SET message_count = (
                SELECT COUNT(*) FROM messages WHERE session_id = ?1
             ) WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .expect("update message count");
    }

    fn result_ids(output: &str) -> Vec<String> {
        let parsed: Value = serde_json::from_str(output).expect("json output");
        parsed["results"]
            .as_array()
            .expect("results")
            .iter()
            .filter_map(|item| {
                item.get("session_id")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
            })
            .collect()
    }

    fn parsed(output: &str) -> Value {
        serde_json::from_str(output).expect("json output")
    }

    #[test]
    fn format_timestamp_handles_none() {
        assert_eq!(
            SqliteSessionSearchBackend::format_timestamp(None),
            "unknown"
        );
    }

    #[test]
    fn format_timestamp_handles_unix_number() {
        let out = SqliteSessionSearchBackend::format_timestamp(Some("1700000000"));
        assert!(out.contains("2023") || out.contains("2024"));
        assert!(out.contains(" at "));
    }

    #[test]
    fn format_timestamp_handles_rfc3339() {
        let out = SqliteSessionSearchBackend::format_timestamp(Some("2026-01-02T03:04:05Z"));
        assert!(out.contains("2026"));
        assert!(out.contains(" at "));
    }

    #[tokio::test]
    async fn recent_mode_keeps_marked_branch_visible_after_parent_reended() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("tui_shutdown"),
            Some("2026-01-01T00:00:20Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_session(
            &conn,
            "branch",
            Some("parent"),
            Some(r#"{"_branched_from":"parent"}"#),
            None,
            None,
            "2026-01-01T00:00:10Z",
        );
        insert_message(&conn, "branch", "user", "branch work must stay visible");

        let output = backend.search(None, None, 5, None).await.expect("recent");
        let ids = result_ids(&output);

        assert!(ids.contains(&"branch".to_string()), "{output}");
    }

    #[tokio::test]
    async fn recent_mode_keeps_legacy_branch_visible() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("branched"),
            Some("2026-01-01T00:00:05Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_session(
            &conn,
            "legacy-branch",
            Some("parent"),
            None,
            None,
            None,
            "2026-01-01T00:00:06Z",
        );
        insert_message(&conn, "legacy-branch", "user", "legacy branch work");

        let output = backend.search(None, None, 5, None).await.expect("recent");
        let ids = result_ids(&output);

        assert!(ids.contains(&"legacy-branch".to_string()), "{output}");
    }

    #[tokio::test]
    async fn recent_mode_hides_unmarked_child_sessions() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("completed"),
            Some("2026-01-01T00:00:05Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_session(
            &conn,
            "subagent-child",
            Some("parent"),
            None,
            None,
            None,
            "2026-01-01T00:00:01Z",
        );
        insert_message(&conn, "subagent-child", "assistant", "internal child work");

        let output = backend.search(None, None, 5, None).await.expect("recent");
        let ids = result_ids(&output);

        assert!(!ids.contains(&"subagent-child".to_string()), "{output}");
    }

    #[tokio::test]
    async fn search_keeps_marked_branch_result_on_branch_session() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "parent",
            None,
            None,
            Some("tui_shutdown"),
            Some("2026-01-01T00:00:20Z"),
            "2026-01-01T00:00:00Z",
        );
        insert_message(&conn, "parent", "user", "parent only message");
        insert_session(
            &conn,
            "branch",
            Some("parent"),
            Some(r#"{"_branched_from":"parent"}"#),
            None,
            None,
            "2026-01-01T00:00:10Z",
        );
        insert_message(&conn, "branch", "user", "unique branch phrase");

        let output = backend
            .search(Some("unique"), None, 5, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids.first().map(String::as_str), Some("branch"), "{output}");
    }

    #[tokio::test]
    async fn search_matches_session_id_before_content_hits() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "20260603_090200_exact",
            None,
            None,
            None,
            None,
            "2026-06-03T09:02:00Z",
        );
        insert_message(
            &conn,
            "20260603_090200_exact",
            "user",
            "ordinary message without the numeric query",
        );
        insert_session(
            &conn,
            "content-session",
            None,
            None,
            None,
            None,
            "2026-06-03T09:03:00Z",
        );
        insert_message(
            &conn,
            "content-session",
            "assistant",
            "20260603 appears only in this transcript",
        );

        let output = backend
            .search(Some("20260603"), None, 2, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(
            ids,
            vec![
                "20260603_090200_exact".to_string(),
                "content-session".to_string()
            ],
            "{output}"
        );
    }

    #[tokio::test]
    async fn no_fts_backend_still_supports_session_id_lookup() {
        let backend = no_fts_backend_with_seeded_db();

        let output = backend
            .search(Some("nofts-session"), None, 5, None)
            .await
            .expect("search");
        let value = parsed(&output);

        assert_eq!(result_ids(&output), vec!["nofts-session".to_string()]);
        assert_eq!(value["search_degraded"], "fts5_unavailable");
    }

    #[tokio::test]
    async fn no_fts_backend_degrades_content_search_to_empty_success() {
        let backend = no_fts_backend_with_seeded_db();

        let output = backend
            .search(Some("uniquephrase"), None, 5, None)
            .await
            .expect("search");
        let value = parsed(&output);

        assert_eq!(value["success"], true);
        assert_eq!(value["count"], 0);
        assert_eq!(value["search_degraded"], "fts5_unavailable");
    }

    #[tokio::test]
    async fn search_dedupes_session_id_and_content_hits_by_logical_session() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "20260603_090200_exact",
            None,
            None,
            None,
            None,
            "2026-06-03T09:02:00Z",
        );
        insert_message(
            &conn,
            "20260603_090200_exact",
            "user",
            "20260603 also appears in the content hit",
        );

        let output = backend
            .search(Some("20260603"), None, 5, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["20260603_090200_exact".to_string()], "{output}");
    }

    #[tokio::test]
    async fn search_session_id_resolves_compression_root_to_tip() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "20260602_235959_root99",
            None,
            None,
            Some("compression"),
            Some("2026-06-03T00:00:05Z"),
            "2026-06-02T23:59:59Z",
        );
        insert_message(&conn, "20260602_235959_root99", "user", "root segment");
        insert_session(
            &conn,
            "20260603_010000_tip01",
            Some("20260602_235959_root99"),
            None,
            None,
            None,
            "2026-06-03T00:00:06Z",
        );
        insert_message(&conn, "20260603_010000_tip01", "user", "continued segment");

        let output = backend
            .search(Some("root99"), None, 1, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["20260603_010000_tip01".to_string()], "{output}");
    }

    #[tokio::test]
    async fn search_session_id_treats_like_wildcards_literally() {
        let (tmp, backend) = backend_with_db();
        let conn = db_conn(&tmp);
        insert_session(
            &conn,
            "literal%id",
            None,
            None,
            None,
            None,
            "2026-06-03T09:02:00Z",
        );
        insert_message(&conn, "literal%id", "user", "literal percent id");
        insert_session(
            &conn,
            "literalXid",
            None,
            None,
            None,
            None,
            "2026-06-03T09:03:00Z",
        );
        insert_message(&conn, "literalXid", "user", "wildcard should not match");

        let output = backend
            .search(Some("%"), None, 1, None)
            .await
            .expect("search");
        let ids = result_ids(&output);

        assert_eq!(ids, vec!["literal%id".to_string()], "{output}");
    }
}
