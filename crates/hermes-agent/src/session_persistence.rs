//! Session persistence — save and load conversation sessions.
//!
//! Provides SQLite-backed session storage with FTS5 indexing for search,
//! human-readable markdown session logs, and trajectory format for RL training.
//!
//! Corresponds to Python `run_agent.py`'s `_persist_session`, `_save_session_log`,
//! and `_save_trajectory` methods.

use std::path::{Path, PathBuf};

use chrono::Utc;
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

impl SessionPersistence {
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
                system_prompt TEXT
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
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id);

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
        .map_err(|e| AgentError::Io(format!("Failed to create tables: {e}")))?;

        if let Err(e) = conn.execute("ALTER TABLE sessions ADD COLUMN system_prompt TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate sessions.system_prompt: {e}"
                )));
            }
        }
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN reasoning_content TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(AgentError::Io(format!(
                    "Failed to migrate messages.reasoning_content: {e}"
                )));
            }
        }

        Ok(())
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
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                updated_at = ?4,
                message_count = ?6,
                title = COALESCE(?5, sessions.title),
                system_prompt = COALESCE(?7, sessions.system_prompt)",
            rusqlite::params![
                session_id,
                model.unwrap_or("unknown"),
                platform.unwrap_or("cli"),
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

    /// Batch insert messages into the database for FTS5 indexing.
    fn flush_messages_to_session_db(
        &self,
        conn: &rusqlite::Connection,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), AgentError> {
        // Delete existing messages for this session (full replace)
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            rusqlite::params![session_id],
        )
        .map_err(|e| AgentError::Io(format!("Failed to clear old messages: {e}")))?;

        let now = Utc::now().to_rfc3339();

        let mut stmt = conn
            .prepare(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, reasoning_content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT role, content, tool_call_id, tool_calls, reasoning_content
                 FROM messages
                 WHERE session_id = ?1
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
}
