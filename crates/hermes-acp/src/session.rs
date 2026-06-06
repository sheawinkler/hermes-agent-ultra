//! ACP session state management.
//!
//! Maps ACP sessions to Hermes agent instances with persistence support.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub profile: Option<String>,
    pub home: Option<String>,
    pub phase: SessionPhase,
    pub history: Vec<Value>,
    pub mode: Option<String>,
    pub config_options: HashMap<String, String>,
    /// Prompts queued by `/queue` to run after the current prompt completes.
    pub queued_prompts: Vec<String>,
    /// Last interrupted prompt text that `/steer` can replay with guidance.
    pub interrupted_prompt_text: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Total prompt tokens across all turns.
    pub total_prompt_tokens: u64,
    /// Total completion tokens across all turns.
    pub total_completion_tokens: u64,
}

/// Public metadata update for ACP sessions.
///
/// Optional fields intentionally preserve existing values when omitted. This
/// mirrors upstream's `update_session_meta(..., model=None)` COALESCE behavior
/// without exposing storage internals to callers.
#[derive(Debug, Clone, Default)]
pub struct SessionMetaUpdate {
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub api_mode: Option<String>,
    pub base_url: Option<String>,
    pub profile: Option<String>,
    pub home: Option<String>,
    pub config_options: HashMap<String, String>,
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
            profile: None,
            home: None,
            phase: SessionPhase::Created,
            history: Vec::new(),
            mode: None,
            config_options: HashMap::new(),
            queued_prompts: Vec::new(),
            interrupted_prompt_text: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
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
            profile: s.profile.clone(),
            home: s.home.clone(),
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
        self.create_session_with_meta(cwd, SessionMetaUpdate::default())
    }

    /// Create a new session with initial metadata.
    pub fn create_session_with_meta(&self, cwd: &str, update: SessionMetaUpdate) -> SessionState {
        let session_id = uuid::Uuid::new_v4().to_string();
        let mut state = SessionState::new(session_id.clone(), cwd.to_string());
        apply_session_meta(&mut state, update);
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
        self.update_session_meta(
            session_id,
            SessionMetaUpdate {
                cwd: Some(cwd.to_string()),
                ..SessionMetaUpdate::default()
            },
        )
    }

    /// Update a session's model identifier.
    pub fn update_model(&self, session_id: &str, model: &str) -> Option<SessionState> {
        self.update_session_meta(
            session_id,
            SessionMetaUpdate {
                model: Some(model.to_string()),
                ..SessionMetaUpdate::default()
            },
        )
    }

    /// Update session metadata through the public, lock-protected manager path.
    pub fn update_session_meta(
        &self,
        session_id: &str,
        update: SessionMetaUpdate,
    ) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            apply_session_meta(state, update);
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

    /// Queue a prompt to execute after the current turn.
    pub fn push_queued_prompt(&self, session_id: &str, prompt: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                state.queued_prompts.push(prompt.to_string());
                state.touch();
            }
            return true;
        }
        false
    }

    /// Drain queued prompts in FIFO order.
    pub fn take_queued_prompts(&self, session_id: &str) -> Vec<String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.touch();
            return std::mem::take(&mut state.queued_prompts);
        }
        Vec::new()
    }

    /// Pop one queued prompt in FIFO order.
    pub fn pop_queued_prompt(&self, session_id: &str) -> Option<String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            if !state.queued_prompts.is_empty() {
                state.touch();
                return Some(state.queued_prompts.remove(0));
            }
        }
        None
    }

    /// Store an interrupted prompt for subsequent `/steer` replay.
    pub fn set_interrupted_prompt_text(&self, session_id: &str, prompt: Option<String>) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.interrupted_prompt_text = prompt
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            state.touch();
            return true;
        }
        false
    }

    /// Take and clear the interrupted prompt text.
    pub fn take_interrupted_prompt_text(&self, session_id: &str) -> Option<String> {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.touch();
            return state.interrupted_prompt_text.take();
        }
        None
    }

    /// Fork a session — deep-copy history into a new session.
    pub fn fork_session(&self, session_id: &str, cwd: &str) -> Option<SessionState> {
        self.fork_session_with_meta(session_id, cwd, SessionMetaUpdate::default())
    }

    /// Fork a session with optional metadata overrides.
    pub fn fork_session_with_meta(
        &self,
        session_id: &str,
        cwd: &str,
        update: SessionMetaUpdate,
    ) -> Option<SessionState> {
        let original = self.get_session(session_id)?;
        let new_id = uuid::Uuid::new_v4().to_string();
        let mut new_state = SessionState::new(new_id.clone(), cwd.to_string());
        new_state.model = original.model.clone();
        new_state.provider = original.provider.clone();
        new_state.api_mode = original.api_mode.clone();
        new_state.base_url = original.base_url.clone();
        new_state.profile = original.profile.clone();
        new_state.home = original.home.clone();
        new_state.mode = original.mode.clone();
        new_state.config_options = original.config_options.clone();
        new_state.history = original.history.clone();
        apply_session_meta(&mut new_state, update);
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

    /// List all session states.
    pub fn list_session_states(&self) -> Vec<SessionState> {
        let sessions = self.sessions.lock().unwrap();
        sessions.values().cloned().collect()
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

fn normalize_meta_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn apply_session_meta(state: &mut SessionState, update: SessionMetaUpdate) {
    if let Some(cwd) = update.cwd.and_then(normalize_meta_string) {
        state.cwd = cwd;
    }
    if let Some(model) = update.model.and_then(normalize_meta_string) {
        state.model = Some(model);
    }
    if let Some(provider) = update.provider.and_then(normalize_meta_string) {
        state.provider = Some(provider);
    }
    if let Some(api_mode) = update.api_mode.and_then(normalize_meta_string) {
        state.api_mode = Some(api_mode);
    }
    if let Some(base_url) = update.base_url.and_then(normalize_meta_string) {
        state.base_url = Some(base_url);
    }
    if let Some(profile) = update.profile.and_then(normalize_meta_string) {
        state.profile = Some(profile);
    }
    if let Some(home) = update.home.and_then(normalize_meta_string) {
        state.home = Some(home);
    }
    for (key, value) in update.config_options {
        let key = key.trim();
        if !key.is_empty() {
            state.config_options.insert(key.to_string(), value);
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

    #[test]
    fn update_session_meta_preserves_model_when_omitted() {
        let mgr = SessionManager::new();
        let state = mgr.create_session("/tmp");
        let sid = state.session_id;

        mgr.update_model(&sid, "nous:gpt-5.4");
        let updated = mgr
            .update_session_meta(
                &sid,
                SessionMetaUpdate {
                    cwd: Some("/workspace".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("session exists");

        assert_eq!(updated.cwd, "/workspace");
        assert_eq!(updated.model.as_deref(), Some("nous:gpt-5.4"));
        let stored = mgr.get_session(&sid).expect("session exists");
        assert_eq!(stored.model.as_deref(), Some("nous:gpt-5.4"));
    }

    #[test]
    fn update_session_meta_merges_fields_and_persists_once() {
        use std::sync::{Arc, Mutex};

        let persisted = Arc::new(Mutex::new(Vec::<SessionState>::new()));
        let persisted_for_cb = persisted.clone();
        let mgr = SessionManager::new().with_persist_callback(move |state| {
            persisted_for_cb
                .lock()
                .expect("persisted lock")
                .push(state.clone());
        });
        let state = mgr.create_session("/tmp");
        let sid = state.session_id;
        persisted.lock().expect("persisted lock").clear();

        let mut config_options = HashMap::new();
        config_options.insert("temperature".to_string(), "0.2".to_string());
        config_options.insert("".to_string(), "ignored".to_string());
        let updated = mgr
            .update_session_meta(
                &sid,
                SessionMetaUpdate {
                    cwd: Some("/repo".to_string()),
                    model: Some("openai:gpt-4o".to_string()),
                    provider: Some("openai".to_string()),
                    api_mode: Some("responses".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    config_options,
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("session exists");

        assert_eq!(updated.cwd, "/repo");
        assert_eq!(updated.model.as_deref(), Some("openai:gpt-4o"));
        assert_eq!(updated.provider.as_deref(), Some("openai"));
        assert_eq!(updated.api_mode.as_deref(), Some("responses"));
        assert_eq!(
            updated.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            updated
                .config_options
                .get("temperature")
                .map(String::as_str),
            Some("0.2")
        );
        assert!(!updated.config_options.contains_key(""));

        let persisted = persisted.lock().expect("persisted lock");
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].session_id, sid);
        assert_eq!(persisted[0].cwd, "/repo");
    }

    #[test]
    fn profile_home_metadata_flows_through_create_update_and_fork() {
        let mgr = SessionManager::new();
        let state = mgr.create_session_with_meta(
            "/workspace",
            SessionMetaUpdate {
                profile: Some("work".to_string()),
                home: Some("/profiles/work".to_string()),
                provider: Some("openrouter".to_string()),
                ..SessionMetaUpdate::default()
            },
        );
        assert_eq!(state.profile.as_deref(), Some("work"));
        assert_eq!(state.home.as_deref(), Some("/profiles/work"));

        let updated = mgr
            .update_session_meta(
                &state.session_id,
                SessionMetaUpdate {
                    cwd: Some("/workspace/repo".to_string()),
                    profile: Some("research".to_string()),
                    home: Some("/profiles/research".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("session exists");
        assert_eq!(updated.cwd, "/workspace/repo");
        assert_eq!(updated.profile.as_deref(), Some("research"));
        assert_eq!(updated.home.as_deref(), Some("/profiles/research"));
        assert_eq!(updated.provider.as_deref(), Some("openrouter"));

        let forked = mgr
            .fork_session_with_meta(
                &state.session_id,
                "/fork",
                SessionMetaUpdate {
                    profile: Some("scratch".to_string()),
                    home: Some("/profiles/scratch".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .expect("forked");
        assert_eq!(forked.cwd, "/fork");
        assert_eq!(forked.profile.as_deref(), Some("scratch"));
        assert_eq!(forked.home.as_deref(), Some("/profiles/scratch"));
        assert_eq!(forked.provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn update_session_meta_missing_session_is_noop() {
        use std::sync::{Arc, Mutex};

        let persisted = Arc::new(Mutex::new(Vec::<SessionState>::new()));
        let persisted_for_cb = persisted.clone();
        let mgr = SessionManager::new().with_persist_callback(move |state| {
            persisted_for_cb
                .lock()
                .expect("persisted lock")
                .push(state.clone());
        });
        assert!(mgr
            .update_session_meta(
                "missing",
                SessionMetaUpdate {
                    cwd: Some("/repo".to_string()),
                    model: Some("openai:gpt-4o".to_string()),
                    ..SessionMetaUpdate::default()
                },
            )
            .is_none());
        assert!(persisted.lock().expect("persisted lock").is_empty());
    }
}
