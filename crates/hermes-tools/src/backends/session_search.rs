//! Real session search backend using rusqlite with FTS5.

use async_trait::async_trait;
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

impl SqliteSessionSearchBackend {
    fn ensure_parent_session_column(conn: &Connection) -> Result<(), ToolError> {
        match conn.execute(
            "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
            rusqlite::params![],
        ) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column name") {
                    Ok(())
                } else {
                    Err(ToolError::ExecutionFailed(format!(
                        "Failed to ensure parent_session_id column: {}",
                        e
                    )))
                }
            }
        }
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
                parent_session_id TEXT
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
            );
            CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content, session_id, role)
                VALUES (new.id, new.content, new.session_id, new.role);
            END;",
        )
        .map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to ensure session schema: {}", e))
        })?;

        Self::ensure_parent_session_column(&conn)?;

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

        let (tasks, sessions_searched, recent_payload): (Vec<SummaryTask>, usize, Option<Value>) = {
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
                    "SELECT id, title, platform, created_at, updated_at, message_count
                     FROM sessions
                     WHERE (parent_session_id IS NULL OR parent_session_id = '')
                       AND COALESCE(platform, '') NOT IN ({})
                     ORDER BY updated_at DESC
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
                params_values.push(rusqlite::types::Value::Integer(limit as i64));
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
                        ))
                    })
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("Recent sessions query failed: {}", e))
                    })?;

                let current_lineage_root =
                    current_session_id.map(|sid| Self::resolve_to_parent(&conn, sid.trim()));
                let mut results = Vec::new();
                for row in rows.flatten() {
                    let (session_id, title, source, started_at, last_active, message_count) = row;
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
                (Vec::new(), 0, Some(payload))
            } else {
                let hidden_placeholders = HIDDEN_SESSION_SOURCES
                    .iter()
                    .map(|_| "?".to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut sql = String::from(
                    "SELECT m.session_id, s.created_at, s.platform, s.model, bm25(messages_fts) AS rank
                     FROM messages_fts
                     JOIN messages m ON m.id = messages_fts.rowid
                     LEFT JOIN sessions s ON s.id = m.session_id
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
                        ))
                    })
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("Session search query failed: {}", e))
                    })?;

                let mut seen = HashSet::new();
                let current_lineage_root =
                    current_session_id.map(|sid| Self::resolve_to_parent(&conn, sid.trim()));
                let mut tasks = Vec::new();
                for row in rows.flatten() {
                    let (raw_session_id, started_at, source, model) = row;
                    let resolved_session_id = Self::resolve_to_parent(&conn, &raw_session_id);
                    if let Some(ref current_root) = current_lineage_root {
                        if &resolved_session_id == current_root {
                            continue;
                        }
                    }
                    if !seen.insert(resolved_session_id.clone()) {
                        continue;
                    }

                    let mut msg_stmt = conn
                        .prepare(
                            "SELECT role, COALESCE(content, ''), tool_calls
                             FROM messages WHERE session_id = ?1 ORDER BY id ASC",
                        )
                        .map_err(|e| {
                            ToolError::ExecutionFailed(format!(
                                "Failed to prepare messages query: {}",
                                e
                            ))
                        })?;
                    let msg_rows = msg_stmt
                        .query_map(rusqlite::params![resolved_session_id.clone()], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Option<String>>(2)?,
                            ))
                        })
                        .map_err(|e| {
                            ToolError::ExecutionFailed(format!(
                                "Failed to load session messages: {}",
                                e
                            ))
                        })?;
                    let messages: Vec<(String, String, Option<String>)> =
                        msg_rows.flatten().collect();
                    if messages.is_empty() {
                        continue;
                    }
                    let transcript = Self::format_conversation(&messages);
                    let transcript =
                        Self::truncate_around_matches(&transcript, query, MAX_SESSION_CHARS);
                    tasks.push(SummaryTask {
                        session_id: resolved_session_id,
                        source,
                        when: started_at,
                        model,
                        conversation_text: transcript,
                    });
                    if tasks.len() >= limit {
                        break;
                    }
                }
                let searched = seen.len();
                (tasks, searched, None)
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

        Ok(json!({
            "success": true,
            "query": query,
            "results": summaries,
            "count": summaries.len(),
            "sessions_searched": sessions_searched,
        })
        .to_string())
    }
}
