//! ACP session state management.
//!
//! Maps ACP sessions to Hermes agent instances with persistence support.
//! Mirrors the Python `acp_adapter/session.py` implementation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// SessionPhase
// ---------------------------------------------------------------------------

/// Lifecycle phase of an ACP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// Session created, awaiting first prompt.
    Created,
    /// Session is actively processing a prompt.
    Active,
    /// Session is idle, waiting for the next prompt.
    Idle,
    /// Session completed normally.
    Completed,
    /// Session was cancelled by the client.
    Cancelled,
    /// Session encountered an unrecoverable error.
    Failed,
}

impl Default for SessionPhase {
    fn default() -> Self {
        Self::Created
    }
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// Tracks per-session state for an ACP-managed Hermes agent.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub api_mode: Option<String>,
    pub base_url: Option<String>,
    pub phase: SessionPhase,
    pub history: Vec<Value>,
    pub mode: Option<String>,
    pub config_options: HashMap<String, String>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Total prompt tokens across all turns.
    pub total_prompt_tokens: u64,
    /// Total completion tokens across all turns.
    pub total_completion_tokens: u64,
}

impl SessionState {
    pub fn new(session_id: String, cwd: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            session_id,
            cwd,
            model: None,
            provider: None,
            api_mode: None,
            base_url: None,
            phase: SessionPhase::Created,
            history: Vec::new(),
            mode: None,
            config_options: HashMap::new(),
            created_at: now,
            updated_at: now,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }
}

// ---------------------------------------------------------------------------
// SessionInfo (lightweight view for listing)
// ---------------------------------------------------------------------------

/// Lightweight session info returned by `list_sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub phase: SessionPhase,
    pub history_len: usize,
    pub created_at: u64,
    pub updated_at: u64,
}

impl From<&SessionState> for SessionInfo {
    fn from(s: &SessionState) -> Self {
        Self {
            session_id: s.session_id.clone(),
            cwd: s.cwd.clone(),
            model: s.model.clone(),
            phase: s.phase,
            history_len: s.history.len(),
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// SessionManager
// ---------------------------------------------------------------------------

/// Thread-safe manager for ACP sessions.
///
/// Sessions are held in-memory for fast access. A persistence callback can be
/// provided to sync state to a database or disk.
pub struct SessionManager {
    sessions: Mutex<HashMap<String, SessionState>>,
    on_persist: Option<Box<dyn Fn(&SessionState) + Send + Sync>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            on_persist: None,
        }
    }

    /// Set a callback invoked whenever a session is persisted.
    pub fn with_persist_callback(
        mut self,
        cb: impl Fn(&SessionState) + Send + Sync + 'static,
    ) -> Self {
        self.on_persist = Some(Box::new(cb));
        self
    }

    /// Create a new session with a unique ID.
    pub fn create_session(&self, cwd: &str) -> SessionState {
        let session_id = uuid::Uuid::new_v4().to_string();
        let state = SessionState::new(session_id.clone(), cwd.to_string());
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.insert(session_id.clone(), state.clone());
        }
        self.persist(&state);
        tracing::info!("Created ACP session {} (cwd={})", session_id, cwd);
        state
    }

    /// Get a session by ID, or `None` if not found.
    pub fn get_session(&self, session_id: &str) -> Option<SessionState> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session_id).cloned()
    }

    /// Update a session's working directory.
    pub fn update_cwd(&self, session_id: &str, cwd: &str) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.cwd = cwd.to_string();
            state.touch();
            let cloned = state.clone();
            drop(sessions);
            self.persist(&cloned);
            Some(cloned)
        } else {
            None
        }
    }

    /// Update a session's model identifier.
    pub fn update_model(&self, session_id: &str, model: &str) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.model = Some(model.to_string());
            state.touch();
            let cloned = state.clone();
            drop(sessions);
            self.persist(&cloned);
            Some(cloned)
        } else {
            None
        }
    }

    /// Update a session's mode identifier.
    pub fn update_mode(&self, session_id: &str, mode: &str) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.mode = Some(mode.to_string());
            state.touch();
            let cloned = state.clone();
            drop(sessions);
            self.persist(&cloned);
            Some(cloned)
        } else {
            None
        }
    }

    /// Set or replace a session config option.
    pub fn set_config_option(
        &self,
        session_id: &str,
        key: &str,
        value: &str,
    ) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state
                .config_options
                .insert(key.to_string(), value.to_string());
            state.touch();
            let cloned = state.clone();
            drop(sessions);
            self.persist(&cloned);
            Some(cloned)
        } else {
            None
        }
    }

    /// Update a session's phase.
    pub fn set_phase(&self, session_id: &str, phase: SessionPhase) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.phase = phase;
            state.touch();
        }
    }

    /// Update session history.
    pub fn set_history(&self, session_id: &str, history: Vec<Value>) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.history = history;
            state.touch();
        }
    }

    /// Fork a session — deep-copy history into a new session.
    pub fn fork_session(&self, session_id: &str, cwd: &str) -> Option<SessionState> {
        let original = self.get_session(session_id)?;
        let new_id = uuid::Uuid::new_v4().to_string();
        let mut new_state = SessionState::new(new_id.clone(), cwd.to_string());
        new_state.model = original.model.clone();
        new_state.history = original.history.clone();
        {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.insert(new_id.clone(), new_state.clone());
        }
        self.persist(&new_state);
        tracing::info!("Forked ACP session {} -> {}", session_id, new_id);
        Some(new_state)
    }

    /// Remove a session.
    pub fn remove_session(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.remove(session_id).is_some()
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.lock().unwrap();
        sessions.values().map(SessionInfo::from).collect()
    }

    /// Persist a session state via the registered callback.
    pub fn save_session(&self, session_id: &str) {
        if let Some(state) = self.get_session(session_id) {
            self.persist(&state);
        }
    }

    /// Accumulate token usage for a session.
    pub fn add_usage(&self, session_id: &str, prompt_tokens: u64, completion_tokens: u64) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.total_prompt_tokens += prompt_tokens;
            state.total_completion_tokens += completion_tokens;
            state.touch();
        }
    }

    fn persist(&self, state: &SessionState) {
        if let Some(ref cb) = self.on_persist {
            cb(state);
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_get_session() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        assert_eq!(state.cwd, "/tmp");
        assert_eq!(state.phase, SessionPhase::Created);

        let got = mgr.get_session(&state.session_id).unwrap();
        assert_eq!(got.session_id, state.session_id);
    }

    #[test]
    fn test_fork_session() {
        let mgr = SessionManager::new();
        let original = mgr.create_session("/project");
        mgr.set_history(
            &original.session_id,
            vec![serde_json::json!({"role": "user", "content": "hello"})],
        );

        let forked = mgr.fork_session(&original.session_id, "/other").unwrap();
        assert_ne!(forked.session_id, original.session_id);
        assert_eq!(forked.cwd, "/other");
        assert_eq!(forked.history.len(), 1);
    }

    #[test]
    fn test_list_sessions() {
        let mgr = SessionManager::new();
        mgr.create_session("/a");
        mgr.create_session("/b");
        assert_eq!(mgr.list_sessions().len(), 2);
    }

    #[test]
    fn test_remove_session() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        assert!(mgr.remove_session(&state.session_id));
        assert!(mgr.get_session(&state.session_id).is_none());
    }

    #[test]
    fn test_update_model_mode_and_config_option() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        let sid = state.session_id;

        mgr.update_model(&sid, "openai:gpt-4o");
        mgr.update_mode(&sid, "code");
        mgr.set_config_option(&sid, "temperature", "0.2");

        let updated = mgr.get_session(&sid).expect("session should exist");
        assert_eq!(updated.model.as_deref(), Some("openai:gpt-4o"));
        assert_eq!(updated.mode.as_deref(), Some("code"));
        assert_eq!(
            updated
                .config_options
                .get("temperature")
                .map(String::as_str),
            Some("0.2")
        );
    }
}
