impl SessionPersistence {
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

    /// Update lineage/end-state metadata for an existing session row.
    pub fn update_session_lineage(
        &self,
        session_id: &str,
        parent_session_id: Option<&str>,
        end_reason: Option<&str>,
        created_at: Option<&str>,
        ended_at: Option<&str>,
    ) -> Result<bool, AgentError> {
        let session_id = session_id.trim();
        if session_id.is_empty() || !self.db_path.exists() {
            return Ok(false);
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let changed = conn
            .execute(
                "UPDATE sessions
                 SET parent_session_id = ?1,
                     end_reason = ?2,
                     created_at = COALESCE(?3, created_at),
                     updated_at = COALESCE(?3, updated_at),
                     ended_at = ?4
                 WHERE id = ?5",
                rusqlite::params![
                    parent_session_id,
                    end_reason,
                    created_at,
                    ended_at,
                    session_id
                ],
            )
            .map_err(|e| AgentError::Io(format!("Failed to update session lineage: {e}")))?;
        Ok(changed > 0)
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

    /// Follow compression-continuation children to the live resume tip.
    ///
    /// Auto-compression ends the current session with `end_reason='compression'`
    /// and forks a child. The parent can still contain flushed messages, so a
    /// naive resume of the parent id misses turns written to the continuation.
    pub fn get_compression_tip(&self, session_id: &str) -> Result<String, AgentError> {
        let mut current = session_id.trim().to_string();
        if current.is_empty() || !self.db_path.exists() {
            return Ok(current);
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..128 {
            if !seen.insert(current.clone()) {
                break;
            }
            let mut stmt = match conn.prepare(
                "SELECT c.id, c.model_config, c.platform
                 FROM sessions c
                 JOIN sessions p ON p.id = c.parent_session_id
                 WHERE c.parent_session_id = ?1
                   AND p.end_reason = 'compression'
                 ORDER BY
                   CASE
                     WHEN c.end_reason = 'compression' THEN 0
                     WHEN NULLIF(TRIM(c.ended_at), '') IS NULL THEN 1
                     ELSE 2
                   END,
                   COALESCE(
                     (SELECT MAX(m.created_at) FROM messages m WHERE m.session_id = c.id),
                     c.updated_at,
                     c.created_at
                   ) DESC,
                   c.created_at DESC,
                   c.id DESC",
            ) {
                Ok(stmt) => stmt,
                Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                    if message.contains("no such table") || message.contains("no such column") =>
                {
                    return Ok(current);
                }
                Err(e) => {
                    return Err(AgentError::Io(format!(
                        "Failed to prepare compression-tip query: {e}"
                    )));
                }
            };
            let candidates = match stmt.query_map(rusqlite::params![current.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            }) {
                Ok(candidates) => candidates,
                Err(e) => {
                    return Err(AgentError::Io(format!(
                        "Failed to resolve compression tip: {e}"
                    )));
                }
            };
            let mut next = None;
            for candidate in candidates {
                let (child_id, model_config, platform) = candidate.map_err(|e| {
                    AgentError::Io(format!("Failed to read compression-tip candidate: {e}"))
                })?;
                let child_id = child_id.trim().to_string();
                if child_id.is_empty()
                    || Self::is_explicit_non_compression_child(
                        model_config.as_deref(),
                        platform.as_deref(),
                    )
                {
                    continue;
                }
                next = Some(child_id);
                break;
            }
            let Some(next) = next else {
                break;
            };
            if next == current {
                break;
            }
            current = next;
        }
        Ok(current)
    }

    /// Resolve a resume target through compression continuations when present.
    pub fn resolve_resume_session_id(&self, session_id: &str) -> Result<String, AgentError> {
        self.get_compression_tip(session_id)
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
                "INSERT INTO messages (session_id, role, content, tool_call_id, name, tool_calls, reasoning_content, created_at, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
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
                msg.name.as_deref(),
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
                "SELECT role, content, tool_call_id, name, tool_calls, reasoning_content
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
                let name: Option<String> = row.get(3)?;
                let tool_calls_json: Option<String> = row.get(4)?;
                let reasoning_content: Option<String> = row.get(5)?;

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
                    anthropic_content_blocks: None,
                    cache_control: None,
                })
            })
            .map_err(|e| AgentError::Io(format!("Failed to query messages: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AgentError::Io(format!("Failed to read messages: {e}")))?;

        Ok(messages)
    }
}
