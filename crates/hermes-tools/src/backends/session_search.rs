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
        let conn = Connection::open(db_path).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to open sessions DB: {}", e))
        })?;

        // Create FTS5 virtual table if it doesn't exist
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS session_messages USING fts5(
                session_id,
                role,
                content,
                timestamp,
                tokenize='porter unicode61'
            );",
        )
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create FTS5 table: {}", e)))?;

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
    async fn search(&self, query: &str, limit: usize) -> Result<String, ToolError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT session_id, role, content, timestamp, rank
             FROM session_messages
             WHERE session_messages MATCH ?1
             ORDER BY rank
             LIMIT ?2",
            )
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to prepare search query: {}", e))
            })?;

        let results: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![query, limit as i64], |row| {
                Ok(json!({
                    "session_id": row.get::<_, String>(0)?,
                    "role": row.get::<_, String>(1)?,
                    "content": row.get::<_, String>(2)?,
                    "timestamp": row.get::<_, String>(3)?,
                    "rank": row.get::<_, f64>(4)?,
                }))
            })
            .map_err(|e| ToolError::ExecutionFailed(format!("Search query failed: {}", e)))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(json!({
            "results": results,
            "total": results.len(),
            "query": query,
        })
        .to_string())
    }
}
