//! Session management (Requirement 7.3-7.6).
//!
//! Provides per-user and per-chat session tracking with configurable
//! reset policies, cross-platform session continuity, and group-session
//! per-user isolation.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use chrono::Timelike;
use hermes_config::session::{SessionResetPolicy, SessionType};
use hermes_core::types::Message;

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

    /// Optional user-visible title for session lists and status surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

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
            title: None,
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
// SessionContext (dynamic system prompt building)
// ---------------------------------------------------------------------------

/// Dynamic context assembled per-session for prompt construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContext {
    pub session_id: String,
    pub platform: String,
    pub user_id: String,
    pub system_prompt_parts: Vec<String>,
    pub metadata: HashMap<String, String>,
}

impl SessionContext {
    pub fn new(session: &Session) -> Self {
        Self {
            session_id: session.id.clone(),
            platform: session.platform.clone(),
            user_id: session.user_id.clone(),
            system_prompt_parts: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a section to the system prompt.
    pub fn add_prompt_part(&mut self, part: impl Into<String>) {
        self.system_prompt_parts.push(part.into());
    }

    /// Set a metadata value.
    pub fn set_metadata(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.metadata.insert(key.into(), value.into());
    }

    /// Build the assembled system prompt from all parts.
    pub fn build_session_context_prompt(&self) -> String {
        let mut prompt = String::new();

        prompt.push_str(&format!(
            "[Platform: {} | User: {}]\n\n",
            self.platform, self.user_id
        ));

        for (i, part) in self.system_prompt_parts.iter().enumerate() {
            if i > 0 {
                prompt.push_str("\n\n");
            }
            prompt.push_str(part);
        }

        if !self.metadata.is_empty() {
            prompt.push_str("\n\n[Context metadata]\n");
            for (key, value) in &self.metadata {
                prompt.push_str(&format!("- {}: {}\n", key, value));
            }
        }

        prompt
    }
}

// ---------------------------------------------------------------------------
// PII Redaction
// ---------------------------------------------------------------------------

/// A rule for redacting PII from text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionRule {
    pub name: String,
    pub pattern: String,
    pub replacement: String,
}

impl RedactionRule {
    pub fn new(
        name: impl Into<String>,
        pattern: impl Into<String>,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            pattern: pattern.into(),
            replacement: replacement.into(),
        }
    }
}

/// Common built-in redaction rules.
pub fn default_redaction_rules() -> Vec<RedactionRule> {
    // Order matters: more specific patterns first to avoid partial matches
    vec![
        RedactionRule::new(
            "email",
            r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}",
            "[EMAIL]",
        ),
        RedactionRule::new(
            "credit_card",
            r"\b\d{4}[\s-]?\d{4}[\s-]?\d{4}[\s-]?\d{4}\b",
            "[CREDIT_CARD]",
        ),
        RedactionRule::new("ssn", r"\b\d{3}-\d{2}-\d{4}\b", "[SSN]"),
        RedactionRule::new(
            "ip_address",
            r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b",
            "[IP_ADDR]",
        ),
        RedactionRule::new("phone", r"\+\d[\d\s\-()]{7,}\d", "[PHONE]"),
    ]
}

/// Apply redaction rules to a text, returning the redacted version.
pub fn redact_pii(text: &str, rules: &[RedactionRule]) -> String {
    let mut result = text.to_string();
    for rule in rules {
        if let Ok(re) = regex::Regex::new(&rule.pattern) {
            result = re
                .replace_all(&result, rule.replacement.as_str())
                .to_string();
        }
    }
    result
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

    /// Compose the canonical session key for platform/chat/user.
    pub fn compose_session_key(&self, platform: &str, chat_id: &str, user_id: &str) -> String {
        // Keep Slack shared channels per-user isolated to avoid command/runtime
        // state bleed between users in the same channel.
        if is_slack_shared_channel(platform, chat_id) && !user_id.trim().is_empty() {
            return format!("{}:{}:{}", platform, chat_id, user_id);
        }
        let session_type = Self::infer_session_type(chat_id);
        if self.group_sessions_per_user && session_type == SessionType::Group {
            format!("{}:{}:{}", platform, chat_id, user_id)
        } else {
            format!("{}:{}", platform, chat_id)
        }
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

        let session_key = self.compose_session_key(platform, chat_id, user_id);

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

    /// Reset a session by key, clearing all messages and rotating the logical
    /// session id. Returns the pre-reset and post-reset session snapshots.
    pub async fn reset_session_with_snapshots(
        &self,
        session_id: &str,
    ) -> Option<(Session, Session)> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            let old = session.clone();
            session.messages.clear();
            session.id = Uuid::new_v4().to_string();
            session.created_at = Utc::now();
            session.last_active_at = Utc::now();
            return Some((old, session.clone()));
        }
        None
    }

    /// Reset a session by key, clearing all messages.
    pub async fn reset_session(&self, session_id: &str) {
        let _ = self.reset_session_with_snapshots(session_id).await;
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

    /// Replace all messages for a session.
    pub async fn replace_messages(&self, session_id: &str, messages: Vec<Message>) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages = messages;
            session.last_active_at = Utc::now();
            return true;
        }
        false
    }

    /// Replace messages and title metadata when switching the active chat
    /// context to another known session.
    pub async fn replace_messages_and_title(
        &self,
        session_id: &str,
        messages: Vec<Message>,
        title: Option<String>,
    ) -> bool {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages = messages;
            session.title = normalize_session_title(title.as_deref());
            session.last_active_at = Utc::now();
            return true;
        }
        false
    }

    /// Set or clear the user-visible title for a session.
    pub async fn set_title(&self, session_id: &str, title: impl AsRef<str>) -> Option<String> {
        let mut sessions = self.sessions.write().await;
        let normalized = normalize_session_title(Some(title.as_ref()));
        let session = sessions.get_mut(session_id)?;
        session.title = normalized.clone();
        session.last_active_at = Utc::now();
        normalized
    }

    /// Return the user-visible title for a session, if one is set.
    pub async fn get_title(&self, session_id: &str) -> Option<String> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).and_then(|s| s.title.clone())
    }

    /// Pop the latest message from a session.
    pub async fn pop_last_message(&self, session_id: &str) -> Option<Message> {
        let mut sessions = self.sessions.write().await;
        sessions.get_mut(session_id).and_then(|s| {
            let popped = s.messages.pop();
            if popped.is_some() {
                s.last_active_at = Utc::now();
            }
            popped
        })
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

    /// Build a `SessionContext` for the given session, populating it with
    /// metadata from the session's state.
    pub async fn build_session_context(&self, session_id: &str) -> Option<SessionContext> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|session| {
            let mut ctx = SessionContext::new(session);
            ctx.set_metadata("session_type", format!("{:?}", session.session_type));
            ctx.set_metadata("message_count", session.messages.len().to_string());
            ctx.set_metadata("created_at", session.created_at.to_rfc3339());
            if let Some(title) = &session.title {
                ctx.set_metadata("title", title.clone());
            }
            ctx
        })
    }

    /// Flush all in-memory sessions (e.g., before shutdown or persistence).
    ///
    /// Returns all sessions that were flushed.
    pub async fn flush_memories(&self) -> Vec<Session> {
        let mut sessions = self.sessions.write().await;
        let all: Vec<Session> = sessions.values().cloned().collect();
        sessions.clear();

        let mut user_sessions = self.user_sessions.write().await;
        user_sessions.clear();

        all
    }

    /// Return all current session snapshots with their canonical keys.
    pub async fn all_sessions(&self) -> Vec<(String, Session)> {
        let sessions = self.sessions.read().await;
        sessions
            .iter()
            .map(|(key, session)| (key.clone(), session.clone()))
            .collect()
    }

    /// Expire idle sessions according to their reset policy.
    ///
    /// Returns the removed session snapshots with their canonical keys.
    pub async fn expire_idle_session_snapshots(&self) -> Vec<(String, Session)> {
        let mut sessions = self.sessions.write().await;
        let stale_ids: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.should_reset())
            .map(|(id, _)| id.clone())
            .collect();
        let mut expired = Vec::new();
        for id in stale_ids {
            if let Some(session) = sessions.remove(&id) {
                expired.push((id, session));
            }
        }
        drop(sessions);

        if !expired.is_empty() {
            let mut user_sessions = self.user_sessions.write().await;
            for (id, session) in &expired {
                if let Some(ids) = user_sessions.get_mut(&session.user_id) {
                    ids.retain(|existing| existing != id);
                }
            }
        }

        expired
    }

    /// Expire idle sessions according to their reset policy.
    ///
    /// Returns the number of removed sessions.
    pub async fn expire_idle_sessions(&self) -> usize {
        self.expire_idle_session_snapshots().await.len()
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

fn is_slack_shared_channel(platform: &str, chat_id: &str) -> bool {
    if !platform.eq_ignore_ascii_case("slack") {
        return false;
    }
    let id = chat_id.trim();
    if id.is_empty() {
        return false;
    }
    let Some(first) = id.chars().next() else {
        return false;
    };
    matches!(first, 'C' | 'c' | 'G' | 'g')
}

fn normalize_session_title(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
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

        manager.add_message(&sid, Message::user("hello")).await;
        manager
            .add_message(&sid, Message::assistant("hi there"))
            .await;

        let msgs = manager.get_messages(&sid).await;
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_session_context_build_prompt() {
        let session = Session::new(
            "telegram",
            "chat1",
            "user1",
            SessionType::Dm,
            SessionResetPolicy::default(),
        );
        let mut ctx = SessionContext::new(&session);
        ctx.add_prompt_part("You are a helpful assistant.");
        ctx.add_prompt_part("Always be concise.");
        ctx.set_metadata("language", "en".to_string());

        let prompt = ctx.build_session_context_prompt();
        assert!(prompt.contains("telegram"));
        assert!(prompt.contains("user1"));
        assert!(prompt.contains("helpful assistant"));
        assert!(prompt.contains("concise"));
        assert!(prompt.contains("language: en"));
    }

    #[test]
    fn test_redact_pii() {
        let rules = default_redaction_rules();
        let text = "Contact me at john@example.com or call +1-555-123-4567.";
        let redacted = redact_pii(text, &rules);
        assert!(redacted.contains("[EMAIL]"));
        assert!(redacted.contains("[PHONE]"));
        assert!(!redacted.contains("john@example.com"));
    }

    #[test]
    fn test_redact_pii_ssn() {
        let rules = default_redaction_rules();
        let text = "My SSN is 123-45-6789.";
        let redacted = redact_pii(text, &rules);
        assert!(redacted.contains("[SSN]"));
        assert!(!redacted.contains("123-45-6789"));
    }

    #[tokio::test]
    async fn test_flush_memories() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        manager
            .get_or_create_session("discord", "chat2", "user2")
            .await;

        assert_eq!(manager.session_count().await, 2);
        let flushed = manager.flush_memories().await;
        assert_eq!(flushed.len(), 2);
        assert_eq!(manager.session_count().await, 0);
    }

    #[tokio::test]
    async fn test_build_session_context() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        let sid = format!("{}:{}", session.platform, session.chat_id);

        let ctx = manager.build_session_context(&sid).await;
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert_eq!(ctx.platform, "telegram");
        assert_eq!(ctx.user_id, "user1");
    }

    #[tokio::test]
    async fn session_manager_reset_clears_messages() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        let sid = format!("{}:{}", session.platform, session.chat_id);

        manager.add_message(&sid, Message::user("hello")).await;
        assert_eq!(manager.get_messages(&sid).await.len(), 1);

        manager.reset_session(&sid).await;
        assert!(manager.get_messages(&sid).await.is_empty());
    }

    #[tokio::test]
    async fn session_manager_reset_returns_old_and_new_logical_ids() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat-reset-id", "user1")
            .await;
        let sid =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);
        manager.add_message(&sid, Message::user("hello")).await;

        let (old_session, new_session) = manager
            .reset_session_with_snapshots(&sid)
            .await
            .expect("session should reset");

        assert_eq!(old_session.id, session.id);
        assert_ne!(new_session.id, old_session.id);
        assert_eq!(new_session.platform, "telegram");
        assert_eq!(new_session.chat_id, "chat-reset-id");
        assert!(new_session.messages.is_empty());
        assert!(manager.get_messages(&sid).await.is_empty());
    }

    #[tokio::test]
    async fn session_manager_sets_clears_and_copies_title_metadata() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat-title", "user1")
            .await;
        let sid =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);

        let title = manager
            .set_title(&sid, "  Release readiness  ")
            .await
            .expect("session exists");
        assert_eq!(title, "Release readiness");
        assert_eq!(
            manager.get_title(&sid).await.as_deref(),
            Some("Release readiness")
        );

        let ctx = manager
            .build_session_context(&sid)
            .await
            .expect("session context");
        assert_eq!(
            ctx.metadata.get("title").map(String::as_str),
            Some("Release readiness")
        );

        assert!(
            manager
                .replace_messages_and_title(
                    &sid,
                    vec![Message::user("hello")],
                    Some("Copied title".to_string()),
                )
                .await
        );
        assert_eq!(manager.get_messages(&sid).await.len(), 1);
        assert_eq!(
            manager.get_title(&sid).await.as_deref(),
            Some("Copied title")
        );

        assert!(manager.set_title(&sid, "   ").await.is_none());
        assert!(manager.get_title(&sid).await.is_none());
    }

    #[tokio::test]
    async fn session_manager_expire_returns_snapshots_and_cleans_user_index() {
        let config = SessionConfig {
            reset_policy: SessionResetPolicy::Idle { timeout_minutes: 0 },
            ..SessionConfig::default()
        };
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat-expire", "user1")
            .await;
        let sid =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);

        let expired = manager.expire_idle_session_snapshots().await;

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, sid);
        assert_eq!(expired[0].1.id, session.id);
        assert!(manager.get_session(&sid).await.is_none());
        assert!(manager.get_user_sessions("user1").await.is_empty());
    }

    #[test]
    fn compose_session_key_slack_shared_channels_are_per_user() {
        let manager = SessionManager::new(SessionConfig::default());
        assert_eq!(
            manager.compose_session_key("slack", "C123ABC", "U999"),
            "slack:C123ABC:U999"
        );
        assert_eq!(
            manager.compose_session_key("slack", "G123ABC", "U999"),
            "slack:G123ABC:U999"
        );
        assert_eq!(
            manager.compose_session_key("slack", "D123ABC", "U999"),
            "slack:D123ABC"
        );
    }
}
