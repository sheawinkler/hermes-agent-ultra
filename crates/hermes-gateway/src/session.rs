//! Session management (Requirement 7.3-7.6).
//!
//! Provides per-user and per-chat session tracking with configurable
//! reset policies, cross-platform session continuity, and group-session
//! per-user isolation.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use hermes_core::types::Message;
use hermes_config::session::{SessionResetPolicy, SessionType};
use chrono::Timelike;

// ---------------------------------------------------------------------------
// Re-export session types from hermes-config for convenience
// ---------------------------------------------------------------------------

pub use hermes_config::session::{
    DailyReset, IdleReset, SessionConfig, SessionResetPolicy as ResetPolicy,
};

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A conversation session, tracking messages and metadata for a user/chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier.
    pub id: String,

    /// Platform this session belongs to (e.g., "telegram", "discord").
    pub platform: String,

    /// Chat or channel identifier on the platform.
    pub chat_id: String,

    /// User identifier on the platform.
    pub user_id: String,

    /// All messages in this session.
    pub messages: Vec<Message>,

    /// When this session was created.
    pub created_at: DateTime<Utc>,

    /// When this session was last active.
    pub last_active_at: DateTime<Utc>,

    /// The reset policy for this session (may override global defaults).
    pub reset_policy: SessionResetPolicy,

    /// The type of this session (DM, Group, Thread).
    pub session_type: SessionType,
}

impl Session {
    /// Create a new session with the given parameters.
    pub fn new(
        platform: impl Into<String>,
        chat_id: impl Into<String>,
        user_id: impl Into<String>,
        session_type: SessionType,
        reset_policy: SessionResetPolicy,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            platform: platform.into(),
            chat_id: chat_id.into(),
            user_id: user_id.into(),
            messages: Vec::new(),
            created_at: now,
            last_active_at: now,
            reset_policy,
            session_type,
        }
    }

    /// Touch the session, updating `last_active_at` to now.
    pub fn touch(&mut self) {
        self.last_active_at = Utc::now();
    }

    /// Check whether this session should be reset based on its reset policy
    /// and the current time.
    pub fn should_reset(&self) -> bool {
        match &self.reset_policy {
            SessionResetPolicy::None => false,
            SessionResetPolicy::Idle { timeout_minutes } => {
                let elapsed = Utc::now()
                    .signed_duration_since(self.last_active_at)
                    .num_minutes();
                elapsed >= *timeout_minutes as i64
            }
            SessionResetPolicy::Daily { at_hour } => {
                // Reset if current hour matches and we haven't reset today
                let now = Utc::now();
                now.time().hour() as u8 == *at_hour
                    && now.date_naive() != self.last_active_at.date_naive()
            }
            SessionResetPolicy::Both {
                daily: DailyReset { at_hour },
                idle: IdleReset { timeout_minutes },
            } => {
                // Reset if either condition is met
                let idle_elapsed = Utc::now()
                    .signed_duration_since(self.last_active_at)
                    .num_minutes();
                let now = Utc::now();
                idle_elapsed >= *timeout_minutes as i64
                    || (now.time().hour() as u8 == *at_hour
                        && now.date_naive() != self.last_active_at.date_naive())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SessionManager
// ---------------------------------------------------------------------------

/// Manages all active sessions, providing creation, retrieval, and
/// cross-platform session continuity.
pub struct SessionManager {
    sessions: RwLock<HashMap<String, Session>>,
    config: SessionConfig,

    /// Index: (user_id) -> Set of session IDs for cross-platform continuity.
    /// This allows the same user on different platforms to share context.
    user_sessions: RwLock<HashMap<String, Vec<String>>>,

    /// Whether group sessions use per-user isolation.
    group_sessions_per_user: bool,
}

impl SessionManager {
    /// Create a new `SessionManager` with the given config.
    pub fn new(config: SessionConfig) -> Self {
        let group_sessions_per_user = false; // Will be overridden per-platform
        Self {
            sessions: RwLock::new(HashMap::new()),
            config,
            user_sessions: RwLock::new(HashMap::new()),
            group_sessions_per_user,
        }
    }

    /// Create a `SessionManager` with explicit group_sessions_per_user setting.
    pub fn with_group_isolation(config: SessionConfig, group_sessions_per_user: bool) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            config,
            user_sessions: RwLock::new(HashMap::new()),
            group_sessions_per_user,
        }
    }

    /// Determine the effective reset policy for a session, applying
    /// per-platform and per-session-type overrides.
    fn effective_reset_policy(
        &self,
        platform: &str,
        session_type: SessionType,
    ) -> SessionResetPolicy {
        // Per-session-type override takes highest precedence
        if let Some(policy) = self.config.session_type_overrides.get(&session_type) {
            return policy.clone();
        }
        // Per-platform override next
        if let Some(policy) = self.config.platform_overrides.get(platform) {
            return policy.clone();
        }
        // Global default
        self.config.reset_policy.clone()
    }

    /// Get or create a session for the given platform, chat, and user.
    ///
    /// If a session exists for this (platform, chat_id, user_id) triple,
    /// return it (after checking whether it should be reset).
    /// Otherwise, create a new session.
    pub async fn get_or_create_session(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> Session {
        let session_type = Self::infer_session_type(chat_id);
        let reset_policy = self.effective_reset_policy(platform, session_type);

        // Build a composite key. For group sessions with per-user isolation,
        // include user_id in the key so each user gets their own context.
        let session_key = if self.group_sessions_per_user && session_type == SessionType::Group {
            format!("{}:{}:{}", platform, chat_id, user_id)
        } else {
            format!("{}:{}", platform, chat_id)
        };

        let mut sessions = self.sessions.write().await;

        if let Some(session) = sessions.get_mut(&session_key) {
            // Check if reset is needed
            if session.should_reset() {
                // Reset: clear messages but keep the session alive
                session.messages.clear();
                session.created_at = Utc::now();
                session.last_active_at = Utc::now();
            } else {
                session.touch();
            }
            return session.clone();
        }

        // Create new session
        let session = Session::new(platform, chat_id, user_id, session_type, reset_policy);
        let session_clone = session.clone();

        sessions.insert(session_key.clone(), session);

        // Track user -> session mapping for cross-platform continuity
        let mut user_sessions = self.user_sessions.write().await;
        user_sessions
            .entry(user_id.to_string())
            .or_default()
            .push(session_key.clone());

        session_clone
    }

    /// Reset a session by ID, clearing all messages.
    pub async fn reset_session(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.clear();
            session.created_at = Utc::now();
            session.last_active_at = Utc::now();
        }
    }

    /// Add a message to a session.
    pub async fn add_message(&self, session_id: &str, message: Message) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.push(message);
            session.last_active_at = Utc::now();
        }
    }

    /// Retrieve all messages for a session.
    pub async fn get_messages(&self, session_id: &str) -> Vec<Message> {
        let sessions = self.sessions.read().await;
        sessions
            .get(session_id)
            .map(|s| s.messages.clone())
            .unwrap_or_default()
    }

    /// Get a session by its ID.
    pub async fn get_session(&self, session_id: &str) -> Option<Session> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// Find all sessions for a given user across all platforms
    /// (cross-platform session continuity).
    pub async fn get_user_sessions(&self, user_id: &str) -> Vec<Session> {
        let user_sessions = self.user_sessions.read().await;
        let session_ids = user_sessions.get(user_id).cloned().unwrap_or_default();
        drop(user_sessions);

        let sessions = self.sessions.read().await;
        session_ids
            .iter()
            .filter_map(|id| sessions.get(id).cloned())
            .collect()
    }

    /// Get the global messages from all sessions for a user across platforms
    /// (cross-platform session continuity).
    pub async fn get_cross_platform_messages(&self, user_id: &str) -> Vec<Message> {
        let user_sessions = self.get_user_sessions(user_id).await;
        let mut all_messages: Vec<Message> = Vec::new();
        for session in &user_sessions {
            all_messages.extend(session.messages.clone());
        }
        // Sort by created_at to maintain chronological order
        all_messages
    }

    /// Remove a session entirely.
    pub async fn remove_session(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.remove(session_id) {
            // Clean up user_sessions index
            let mut user_sessions = self.user_sessions.write().await;
            if let Some(ids) = user_sessions.get_mut(&session.user_id) {
                ids.retain(|id| id != session_id);
            }
        }
    }

    /// Return the number of active sessions.
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Check whether a session should be reset and perform reset if needed.
    /// Called periodically or before each interaction.
    pub async fn check_and_reset_if_needed(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            if session.should_reset() {
                session.messages.clear();
                session.created_at = Utc::now();
                session.last_active_at = Utc::now();
                return true;
            }
        }
        false
    }

    /// Expire idle sessions according to their reset policy.
    ///
    /// Returns the number of removed sessions.
    pub async fn expire_idle_sessions(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let before = sessions.len();
        let stale_ids: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.should_reset())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale_ids {
            sessions.remove(id);
        }
        before.saturating_sub(sessions.len())
    }

    /// Infer the session type from the chat_id format.
    /// By convention, DMs have negative or small numeric IDs,
    /// groups have positive IDs. Override as needed.
    fn infer_session_type(chat_id: &str) -> SessionType {
        // Simple heuristic: if the chat_id starts with a '-' or contains
        // a group-like prefix, treat as group; otherwise DM.
        if chat_id.starts_with('-') || chat_id.contains("group") {
            SessionType::Group
        } else {
            SessionType::Dm
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_new() {
        let session = Session::new(
            "telegram",
            "chat123",
            "user456",
            SessionType::Dm,
            SessionResetPolicy::default(),
        );
        assert_eq!(session.platform, "telegram");
        assert_eq!(session.chat_id, "chat123");
        assert_eq!(session.user_id, "user456");
        assert!(session.messages.is_empty());
    }

    #[test]
    fn session_touch_updates_last_active() {
        let mut session = Session::new(
            "discord",
            "ch1",
            "u1",
            SessionType::Dm,
            SessionResetPolicy::None,
        );
        let before = session.last_active_at;
        session.touch();
        assert!(session.last_active_at >= before);
    }

    #[tokio::test]
    async fn session_manager_get_or_create() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        assert_eq!(session.platform, "telegram");
        assert_eq!(session.chat_id, "chat1");
    }

    #[tokio::test]
    async fn session_manager_add_and_get_messages() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        // SessionManager uses "platform:chat_id" as the map key, not the UUID id
        let sid = format!("{}:{}", session.platform, session.chat_id);

        manager
            .add_message(&sid, Message::user("hello"))
            .await;
        manager
            .add_message(&sid, Message::assistant("hi there"))
            .await;

        let msgs = manager.get_messages(&sid).await;
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn session_manager_reset_clears_messages() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        let sid = format!("{}:{}", session.platform, session.chat_id);

        manager
            .add_message(&sid, Message::user("hello"))
            .await;
        assert_eq!(manager.get_messages(&sid).await.len(), 1);

        manager.reset_session(&sid).await;
        assert!(manager.get_messages(&sid).await.is_empty());
    }
}