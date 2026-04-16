//! Real session search backend using rusqlite with FTS5.

use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::json;
use std::sync::Mutex;

use crate::tools::session_search::SessionSearchBackend;
use hermes_core::ToolError;

/// Real session search backend using SQLite FTS5.
pub struct SqliteSessionSearchBackend {
    conn: Mutex<Connection>,
}

impl SqliteSessionSearchBackend {
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
                message_count INTEGER DEFAULT 0
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
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                session_id UNINDEXED,
                role UNINDEXED,
                content='messages',
                content_rowid='id'
            );",
        )
        .map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to ensure session schema: {}", e))
        })?;

        Ok(Self {
            conn: Mutex::new(conn),
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
            "INSERT INTO session_messages (session_id, role, content, timestamp) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, role, content, timestamp],
        ).map_err(|e| ToolError::ExecutionFailed(format!("Failed to index message: {}", e)))?;
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
    ) -> Result<String, ToolError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let query = query.map(str::trim).unwrap_or("");
        let limit = limit.min(5).max(1);

        // Recent-mode parity: no query means list recent sessions metadata.
        if query.is_empty() {
            let mut stmt = conn
                .prepare(
                    "SELECT id, title, platform, created_at, updated_at, message_count
                     FROM sessions
                     ORDER BY updated_at DESC
                     LIMIT ?1",
                )
                .map_err(|e| {
                    ToolError::ExecutionFailed(format!(
                        "Failed to prepare recent sessions query: {}",
                        e
                    ))
                })?;
            let rows = stmt
                .query_map(rusqlite::params![limit as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?
                            .unwrap_or_else(|| "cli".to_string()),
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5).unwrap_or(0),
                    ))
                })
                .map_err(|e| {
                    ToolError::ExecutionFailed(format!("Recent sessions query failed: {}", e))
                })?;

            let mut results = Vec::new();
            for row in rows.flatten() {
                let (session_id, title, source, started_at, last_active, message_count) = row;
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
            }
            return Ok(json!({
                "success": true,
                "mode": "recent",
                "results": results,
                "count": results.len(),
                "message": format!(
                    "Showing {} most recent sessions. Use a keyword query to search specific topics.",
                    results.len()
                ),
            }).to_string());
        }

        let mut sql = String::from(
            "SELECT m.session_id, m.content, s.created_at, s.platform, s.model, bm25(messages_fts) AS rank
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             LEFT JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1
               AND m.content IS NOT NULL
               AND m.content != ''",
        );

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
            ToolError::ExecutionFailed(format!("Failed to prepare session search query: {}", e))
        })?;

        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(query.to_string())];
        values.extend(role_values.into_iter().map(rusqlite::types::Value::Text));
        let params = rusqlite::params_from_iter(values.iter());

        let rows = stmt
            .query_map(params, |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?
                        .unwrap_or_else(|| "cli".to_string()),
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Session search query failed: {}", e))
            })?;

        let mut seen = std::collections::HashSet::new();
        let mut summaries = Vec::new();
        for row in rows.flatten() {
            let (session_id, content, started_at, source, model) = row;
            if !seen.insert(session_id.clone()) {
                continue;
            }
            let preview = if content.chars().count() > 500 {
                format!("{}…", content.chars().take(500).collect::<String>())
            } else {
                content
            };
            summaries.push(json!({
                "session_id": session_id,
                "when": started_at,
                "source": source,
                "model": model,
                "summary": format!("[Raw preview — summarization unavailable]\n{}", preview),
            }));
            if summaries.len() >= limit {
                break;
            }
        }

        Ok(json!({
            "success": true,
            "query": query,
            "results": summaries,
            "count": summaries.len(),
            "sessions_searched": seen.len(),
        })
        .to_string())
    }
}
