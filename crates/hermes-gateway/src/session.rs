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
use hermes_core::types::{Message, MessageRole};

const DEFAULT_MAX_CONTEXT_MESSAGES: usize = 2048;
const HARD_MAX_CONTEXT_MESSAGES: usize = 4096;
const HARD_MAX_CONTEXT_BYTES: usize = 16 * 1024 * 1024;
const HARD_MAX_RETAINED_CONTEXT_BYTES: usize = 256 * 1024 * 1024;
const MAX_SESSION_TITLE_CHARS: usize = 256;

// ---------------------------------------------------------------------------
// Re-export session types from hermes-config for convenience
// ---------------------------------------------------------------------------

pub use hermes_config::session::{
    DailyReset, IdleReset, SessionConfig, SessionResetPolicy as ResetPolicy,
};

/// Explicit platform ingress identity used to select one conversation session.
///
/// `parent_chat_id` is the channel or chat whose authorization policy applies;
/// `thread_id` is the platform-native reply target when the message belongs to
/// a threaded conversation. Adapters must construct this from authenticated
/// ingress metadata rather than inferring semantics from identifier strings.
#[derive(Debug, Clone, Copy)]
pub struct ConversationScope<'a> {
    pub platform: &'a str,
    pub parent_chat_id: &'a str,
    pub thread_id: Option<&'a str>,
    pub user_id: &'a str,
    pub is_dm: bool,
}

impl<'a> ConversationScope<'a> {
    pub fn new(
        platform: &'a str,
        parent_chat_id: &'a str,
        thread_id: Option<&'a str>,
        user_id: &'a str,
        is_dm: bool,
    ) -> Self {
        Self {
            platform,
            parent_chat_id,
            thread_id,
            user_id,
            is_dm,
        }
    }

    fn session_type(self) -> SessionType {
        if self
            .thread_id
            .is_some_and(|thread_id| !thread_id.trim().is_empty())
        {
            SessionType::Thread
        } else if self.is_dm {
            SessionType::Dm
        } else {
            SessionType::Group
        }
    }
}

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
    /// Platform-native thread or topic identifier, if this session is scoped
    /// to a threaded conversation beneath `chat_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,

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
            thread_id: None,
            user_id: user_id.into(),
            messages: Vec::new(),
            title: None,
            created_at: now,
            last_active_at: now,
            reset_policy,
            session_type,
        }
    }

    /// Create a session from explicit ingress conversation scope.
    fn new_for_scope(scope: ConversationScope<'_>, reset_policy: SessionResetPolicy) -> Self {
        let mut session = Self::new(
            scope.platform,
            scope.parent_chat_id,
            scope.user_id,
            scope.session_type(),
            reset_policy,
        );
        session.thread_id = scope
            .thread_id
            .map(str::trim)
            .filter(|thread_id| !thread_id.is_empty())
            .map(str::to_string);
        session
    }

    /// Recover this session's explicit conversation boundary.
    pub fn conversation_scope(&self) -> ConversationScope<'_> {
        ConversationScope::new(
            &self.platform,
            &self.chat_id,
            self.thread_id.as_deref(),
            &self.user_id,
            self.session_type == SessionType::Dm,
        )
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

fn json_retained_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            std::mem::size_of_val(value)
        }
        serde_json::Value::String(value) => value.capacity(),
        serde_json::Value::Array(values) => {
            values.capacity() * std::mem::size_of::<serde_json::Value>()
                + values.iter().map(json_retained_bytes).sum::<usize>()
        }
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| key.capacity() + json_retained_bytes(value))
            .sum(),
    }
}

fn message_retained_bytes(message: &Message) -> usize {
    let mut bytes = 0;
    bytes += message.content.as_ref().map_or(0, String::capacity);
    bytes += message.tool_call_id.as_ref().map_or(0, String::capacity);
    bytes += message.name.as_ref().map_or(0, String::capacity);
    bytes += message
        .reasoning_content
        .as_ref()
        .map_or(0, String::capacity);
    if let Some(tool_calls) = message.tool_calls.as_ref() {
        bytes += tool_calls.capacity() * std::mem::size_of::<hermes_core::types::ToolCall>();
        for tool_call in tool_calls {
            bytes += tool_call.id.capacity();
            bytes += tool_call.function.name.capacity();
            bytes += tool_call.function.arguments.capacity();
            bytes += tool_call
                .extra_content
                .as_ref()
                .map_or(0, json_retained_bytes);
        }
    }
    if let Some(blocks) = message.anthropic_content_blocks.as_ref() {
        bytes += blocks.capacity() * std::mem::size_of::<serde_json::Value>();
        bytes += blocks.iter().map(json_retained_bytes).sum::<usize>();
    }
    bytes
}
fn messages_live_bytes(messages: &[Message]) -> usize {
    messages
        .len()
        .saturating_mul(std::mem::size_of::<Message>())
        .saturating_add(
            messages
                .iter()
                .map(message_retained_bytes)
                .fold(0usize, usize::saturating_add),
        )
}

fn messages_retained_bytes(messages: &Vec<Message>) -> usize {
    messages
        .capacity()
        .saturating_mul(std::mem::size_of::<Message>())
        .saturating_add(
            messages
                .iter()
                .map(message_retained_bytes)
                .fold(0usize, usize::saturating_add),
        )
}
fn compact_messages(messages: &mut Vec<Message>) {
    if messages.is_empty() {
        *messages = Vec::new();
    } else {
        *messages = std::mem::take(messages).into_boxed_slice().into_vec();
    }
}

fn remove_oldest_context_turn(messages: &mut Vec<Message>) -> bool {
    let Some(first_user) = messages
        .iter()
        .position(|message| message.role == MessageRole::User)
    else {
        if messages.is_empty() {
            return false;
        }
        messages.remove(0);
        return true;
    };
    let next_user = messages[first_user + 1..]
        .iter()
        .position(|message| message.role == MessageRole::User)
        .map(|offset| first_user + 1 + offset)
        .unwrap_or(messages.len());
    if first_user > 0 && next_user == messages.len() {
        messages.remove(0);
    } else {
        messages.drain(first_user..next_user);
    }
    true
}

// ---------------------------------------------------------------------------
// SessionManager
// ---------------------------------------------------------------------------

/// Manages all active sessions, providing creation, retrieval, and
/// cross-platform session continuity.
#[derive(Default)]
struct SessionStore {
    sessions: HashMap<String, Session>,
    retained_message_bytes: usize,
}

pub struct SessionManager {
    store: RwLock<SessionStore>,
    config: SessionConfig,
    resource_limits_enabled: bool,
}

impl SessionManager {
    /// Create a new `SessionManager` with the given config.
    pub fn new(config: SessionConfig) -> Self {
        Self::with_group_isolation_and_resource_limits(config, false, false)
    }

    /// Create a manager with the implicit hard resource ceilings enabled or disabled.
    pub fn with_resource_limits(config: SessionConfig, resource_limits_enabled: bool) -> Self {
        Self::with_group_isolation_and_resource_limits(config, false, resource_limits_enabled)
    }

    /// Construct a manager for callers retaining the historical constructor.
    ///
    /// Authenticated ingress now always scopes group and threaded traffic by
    /// its verified user and optional thread, so `group_sessions_per_user`
    /// cannot weaken conversation isolation.
    pub fn with_group_isolation(config: SessionConfig, _group_sessions_per_user: bool) -> Self {
        Self::with_group_isolation_and_resource_limits(config, false, false)
    }

    /// Construct a manager with explicit resource-limit settings.
    ///
    /// The group-isolation argument is retained for source compatibility but
    /// explicit ingress scope is always user-isolated.
    pub fn with_group_isolation_and_resource_limits(
        config: SessionConfig,
        _group_sessions_per_user: bool,
        resource_limits_enabled: bool,
    ) -> Self {
        Self {
            store: RwLock::new(SessionStore::default()),
            config,
            resource_limits_enabled,
        }
    }
    fn context_message_limit(&self) -> usize {
        match (
            self.resource_limits_enabled,
            self.config.max_context_messages,
        ) {
            (true, configured) => configured
                .unwrap_or(DEFAULT_MAX_CONTEXT_MESSAGES)
                .min(HARD_MAX_CONTEXT_MESSAGES),
            (false, Some(configured)) => configured,
            (false, None) => usize::MAX,
        }
    }
    fn context_byte_budget(&self, store: &SessionStore, old_bytes: usize) -> (usize, usize) {
        let retained_without_session = store.retained_message_bytes.saturating_sub(old_bytes);
        let available = if self.resource_limits_enabled {
            HARD_MAX_RETAINED_CONTEXT_BYTES
                .saturating_sub(retained_without_session)
                .min(HARD_MAX_CONTEXT_BYTES)
        } else {
            usize::MAX
        };
        (retained_without_session, available)
    }

    fn enforce_message_limits(messages: &mut Vec<Message>, max_messages: usize, max_bytes: usize) {
        if max_messages == 0 || max_bytes == 0 {
            *messages = Vec::new();
            return;
        }

        while messages.len() > max_messages || messages_live_bytes(messages) > max_bytes {
            if !remove_oldest_context_turn(messages) {
                break;
            }
        }

        let material_capacity = max_messages.max(messages.len()).max(4);
        if messages.capacity() > material_capacity.saturating_mul(2)
            || messages_retained_bytes(messages) > max_bytes
        {
            compact_messages(messages);
        }

        while messages_retained_bytes(messages) > max_bytes {
            if !remove_oldest_context_turn(messages) {
                *messages = Vec::new();
                break;
            }
            compact_messages(messages);
        }

        debug_assert!(messages.len() <= max_messages);
        debug_assert!(messages_retained_bytes(messages) <= max_bytes);
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

    /// Compose the canonical key for explicit platform ingress scope.
    ///
    /// Every dimension is length-delimited so a platform identifier, parent
    /// chat, thread/topic, or user ID cannot collide through delimiters. This
    /// is the only key API used by routed ingress.
    pub fn compose_session_key_for_scope(&self, scope: ConversationScope<'_>) -> String {
        fn append_component(key: &mut String, value: &str) {
            key.push(':');
            key.push_str(&value.len().to_string());
            key.push(':');
            key.push_str(value);
        }

        let mut key = String::from("scope:v1");
        append_component(&mut key, scope.platform);
        append_component(&mut key, scope.parent_chat_id);
        append_component(
            &mut key,
            scope
                .thread_id
                .map(str::trim)
                .filter(|thread_id| !thread_id.is_empty())
                .unwrap_or_default(),
        );
        append_component(&mut key, scope.user_id);
        key
    }

    /// Compose a direct-message key for callers that have no explicit
    /// platform conversation metadata. Production ingress must use
    /// [`Self::compose_session_key_for_scope`] instead.
    pub fn compose_session_key(&self, platform: &str, chat_id: &str, user_id: &str) -> String {
        self.compose_session_key_for_scope(ConversationScope::new(
            platform, chat_id, None, user_id, true,
        ))
    }

    /// Get or create a session for explicit, authenticated ingress scope.
    pub async fn get_or_create_session_for_scope(&self, scope: ConversationScope<'_>) -> Session {
        self.try_get_or_create_session_for_scope(scope, usize::MAX)
            .await
            .expect("unbounded session creation cannot be rejected")
    }

    /// Get or create an explicit ingress session without exceeding the
    /// retained-session cap.
    pub async fn try_get_or_create_session_for_scope(
        &self,
        scope: ConversationScope<'_>,
        max_sessions: usize,
    ) -> Option<Session> {
        let session_key = self.compose_session_key_for_scope(scope);
        self.try_get_or_create_session_with_key(scope, session_key, max_sessions)
            .await
    }

    /// Get or create a legacy direct-message session. Callers that receive a
    /// platform event must use [`Self::get_or_create_session_for_scope`] so
    /// `is_dm` and thread identity are never guessed from string IDs.
    pub async fn get_or_create_session(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> Session {
        self.try_get_or_create_session(platform, chat_id, user_id, usize::MAX)
            .await
            .expect("unbounded session creation cannot be rejected")
    }

    /// Legacy direct-message session creation without exceeding the retained
    /// session cap. New platform ingress must use the scope variant above.
    pub async fn try_get_or_create_session(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
        max_sessions: usize,
    ) -> Option<Session> {
        let scope = ConversationScope::new(platform, chat_id, None, user_id, true);
        let session_key = self.compose_session_key(platform, chat_id, user_id);
        self.try_get_or_create_session_with_key(scope, session_key, max_sessions)
            .await
    }

    async fn try_get_or_create_session_with_key(
        &self,
        scope: ConversationScope<'_>,
        session_key: String,
        max_sessions: usize,
    ) -> Option<Session> {
        let reset_policy = self.effective_reset_policy(scope.platform, scope.session_type());
        let mut store = self.store.write().await;
        if store.sessions.contains_key(&session_key) {
            let old_bytes = store
                .sessions
                .get(&session_key)
                .map(|session| messages_retained_bytes(&session.messages))
                .unwrap_or(0);
            let (session, reset) = {
                let session = store
                    .sessions
                    .get_mut(&session_key)
                    .expect("existing session disappeared under write lock");
                let reset = session.should_reset();
                if reset {
                    session.messages = Vec::new();
                    session.created_at = Utc::now();
                    session.last_active_at = Utc::now();
                } else {
                    session.touch();
                }
                (session.clone(), reset)
            };
            if reset {
                store.retained_message_bytes =
                    store.retained_message_bytes.saturating_sub(old_bytes);
            }
            return Some(session);
        }
        if store.sessions.len() >= max_sessions {
            return None;
        }

        let session = Session::new_for_scope(scope, reset_policy);
        let session_clone = session.clone();
        store.sessions.insert(session_key, session);
        Some(session_clone)
    }

    /// Reset a session by key, clearing all messages and rotating the logical
    /// session id. Returns the pre-reset and post-reset session snapshots.
    pub async fn reset_session_with_snapshots(
        &self,
        session_id: &str,
    ) -> Option<(Session, Session)> {
        let mut store = self.store.write().await;
        let old_bytes = store
            .sessions
            .get(session_id)
            .map(|session| messages_retained_bytes(&session.messages))?;
        let snapshots = {
            let session = store.sessions.get_mut(session_id)?;
            let old = session.clone();
            session.messages = Vec::new();
            session.id = Uuid::new_v4().to_string();
            session.created_at = Utc::now();
            session.last_active_at = Utc::now();
            (old, session.clone())
        };
        store.retained_message_bytes = store.retained_message_bytes.saturating_sub(old_bytes);
        Some(snapshots)
    }

    /// Reset a session by key, clearing all messages.
    pub async fn reset_session(&self, session_id: &str) {
        let _ = self.reset_session_with_snapshots(session_id).await;
    }

    /// Add a message, applying any configured or opted-in retention policy.
    pub async fn add_message(&self, session_id: &str, message: Message) {
        let _ = self.try_add_message(session_id, message).await;
    }

    /// Add an ingress user message only when the current turn can be retained.
    pub async fn try_add_user_message(&self, session_id: &str, message: Message) -> bool {
        if message.role != MessageRole::User {
            return false;
        }
        self.try_add_message(session_id, message).await
    }

    async fn try_add_message(&self, session_id: &str, message: Message) -> bool {
        let max_messages = self.context_message_limit();
        let mut store = self.store.write().await;
        let Some(old_bytes) = store
            .sessions
            .get(session_id)
            .map(|session| messages_retained_bytes(&session.messages))
        else {
            return false;
        };
        let (retained_without_session, max_bytes) = self.context_byte_budget(&store, old_bytes);
        let message_bytes =
            std::mem::size_of::<Message>().saturating_add(message_retained_bytes(&message));
        if max_messages == 0 || message_bytes > max_bytes {
            return false;
        }
        let new_bytes = {
            let session = store
                .sessions
                .get_mut(session_id)
                .expect("existing session disappeared under write lock");
            session.messages.push(message);
            Self::enforce_message_limits(&mut session.messages, max_messages, max_bytes);
            session.last_active_at = Utc::now();
            messages_retained_bytes(&session.messages)
        };
        store.retained_message_bytes = retained_without_session.saturating_add(new_bytes);
        true
    }

    /// Retrieve all messages for a session.
    pub async fn get_messages(&self, session_id: &str) -> Vec<Message> {
        let store = self.store.read().await;
        store
            .sessions
            .get(session_id)
            .map(|s| s.messages.clone())
            .unwrap_or_default()
    }

    /// Replace all messages for a session.
    pub async fn replace_messages(&self, session_id: &str, mut messages: Vec<Message>) -> bool {
        let max_messages = self.context_message_limit();
        let mut store = self.store.write().await;
        let Some(old_bytes) = store
            .sessions
            .get(session_id)
            .map(|session| messages_retained_bytes(&session.messages))
        else {
            return false;
        };
        let (retained_without_session, max_bytes) = self.context_byte_budget(&store, old_bytes);
        Self::enforce_message_limits(&mut messages, max_messages, max_bytes);
        let new_bytes = messages_retained_bytes(&messages);
        let session = store
            .sessions
            .get_mut(session_id)
            .expect("existing session disappeared under write lock");
        session.messages = messages;
        session.last_active_at = Utc::now();
        store.retained_message_bytes = retained_without_session.saturating_add(new_bytes);
        true
    }

    pub(crate) async fn restore_session_snapshot(
        &self,
        session_key: &str,
        snapshot: Option<Session>,
    ) {
        let max_messages = self.context_message_limit();
        let mut store = self.store.write().await;
        let old_bytes = store
            .sessions
            .get(session_key)
            .map(|session| messages_retained_bytes(&session.messages))
            .unwrap_or(0);
        let (retained_without_session, max_bytes) = self.context_byte_budget(&store, old_bytes);
        match snapshot {
            Some(mut session) => {
                Self::enforce_message_limits(&mut session.messages, max_messages, max_bytes);
                let new_bytes = messages_retained_bytes(&session.messages);
                store.sessions.insert(session_key.to_string(), session);
                store.retained_message_bytes = retained_without_session.saturating_add(new_bytes);
            }
            None => {
                store.sessions.remove(session_key);
                store.retained_message_bytes = retained_without_session;
            }
        }
    }

    /// Replace messages and title metadata when switching the active chat
    /// context to another known session.
    pub async fn replace_messages_and_title(
        &self,
        session_id: &str,
        mut messages: Vec<Message>,
        title: Option<String>,
    ) -> bool {
        let max_messages = self.context_message_limit();
        let mut store = self.store.write().await;
        let Some(old_bytes) = store
            .sessions
            .get(session_id)
            .map(|session| messages_retained_bytes(&session.messages))
        else {
            return false;
        };
        let (retained_without_session, max_bytes) = self.context_byte_budget(&store, old_bytes);
        Self::enforce_message_limits(&mut messages, max_messages, max_bytes);
        let new_bytes = messages_retained_bytes(&messages);
        let session = store
            .sessions
            .get_mut(session_id)
            .expect("existing session disappeared under write lock");
        session.messages = messages;
        session.title = normalize_session_title(title.as_deref());
        session.last_active_at = Utc::now();
        store.retained_message_bytes = retained_without_session.saturating_add(new_bytes);
        true
    }

    /// Set or clear the user-visible title for a session.
    pub async fn set_title(&self, session_id: &str, title: impl AsRef<str>) -> Option<String> {
        let mut store = self.store.write().await;
        let normalized = normalize_session_title(Some(title.as_ref()));
        let session = store.sessions.get_mut(session_id)?;
        session.title = normalized.clone();
        session.last_active_at = Utc::now();
        normalized
    }

    /// Return the user-visible title for a session, if one is set.
    pub async fn get_title(&self, session_id: &str) -> Option<String> {
        let store = self.store.read().await;
        store.sessions.get(session_id).and_then(|s| s.title.clone())
    }

    /// Pop the latest message from a session.
    pub async fn pop_last_message(&self, session_id: &str) -> Option<Message> {
        let mut store = self.store.write().await;
        let old_bytes = store
            .sessions
            .get(session_id)
            .map(|session| messages_retained_bytes(&session.messages))?;
        let (popped, new_bytes) = {
            let session = store.sessions.get_mut(session_id)?;
            let popped = session.messages.pop();
            if popped.is_some() {
                session.last_active_at = Utc::now();
            }
            let new_bytes = messages_retained_bytes(&session.messages);
            (popped, new_bytes)
        };
        store.retained_message_bytes = store
            .retained_message_bytes
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes);
        popped
    }

    /// Remove up to `count` latest messages under one write lock.
    pub(crate) async fn remove_last_messages(&self, session_id: &str, count: usize) -> usize {
        if count == 0 {
            return 0;
        }
        let mut store = self.store.write().await;
        let Some(old_bytes) = store
            .sessions
            .get(session_id)
            .map(|session| messages_retained_bytes(&session.messages))
        else {
            return 0;
        };
        let (removed, new_bytes) = {
            let session = store
                .sessions
                .get_mut(session_id)
                .expect("existing session disappeared under write lock");
            let removed = count.min(session.messages.len());
            session.messages.truncate(session.messages.len() - removed);
            if removed > 0 {
                session.last_active_at = Utc::now();
            }
            (removed, messages_retained_bytes(&session.messages))
        };
        store.retained_message_bytes = store
            .retained_message_bytes
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes);
        removed
    }

    /// Undo the latest message and its predecessor when the latest is an assistant reply.
    pub(crate) async fn undo_last_exchange(&self, session_id: &str) -> usize {
        let mut store = self.store.write().await;
        let Some(old_bytes) = store
            .sessions
            .get(session_id)
            .map(|session| messages_retained_bytes(&session.messages))
        else {
            return 0;
        };
        let (removed, new_bytes) = {
            let session = store
                .sessions
                .get_mut(session_id)
                .expect("existing session disappeared under write lock");
            let mut removed = 0;
            if let Some(last) = session.messages.pop() {
                removed = 1;
                if last.role == MessageRole::Assistant
                    && session
                        .messages
                        .last()
                        .is_some_and(|previous| previous.role == MessageRole::User)
                {
                    session.messages.pop();
                    removed += 1;
                }
                session.last_active_at = Utc::now();
            }
            (removed, messages_retained_bytes(&session.messages))
        };
        store.retained_message_bytes = store
            .retained_message_bytes
            .saturating_sub(old_bytes)
            .saturating_add(new_bytes);
        removed
    }

    /// Get a session by its ID.
    pub async fn get_session(&self, session_id: &str) -> Option<Session> {
        let store = self.store.read().await;
        store.sessions.get(session_id).cloned()
    }

    /// Find all sessions for a given user across all platforms
    /// (cross-platform session continuity).
    pub async fn get_user_sessions(&self, user_id: &str) -> Vec<Session> {
        let store = self.store.read().await;
        let mut sessions = store
            .sessions
            .values()
            .filter(|session| session.user_id == user_id)
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        sessions
    }

    /// Search a user's visible sessions by title, UUID id, or canonical gateway key.
    pub async fn search_user_sessions(
        &self,
        user_id: &str,
        query: &str,
        limit: usize,
    ) -> Vec<Session> {
        let query = query.trim();
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }

        let needle = query.to_ascii_lowercase();
        let normalized_needle = normalize_session_search_text(query);
        let user_sessions = self.get_user_sessions(user_id).await;
        let mut matches = user_sessions
            .into_iter()
            .filter(|session| {
                let key = self.compose_session_key_for_scope(session.conversation_scope());
                session_matches_query(session, &key, &needle, &normalized_needle)
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|session| std::cmp::Reverse(session.last_active_at));
        matches.truncate(limit);
        matches
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
        let mut store = self.store.write().await;
        if let Some(session) = store.sessions.remove(session_id) {
            store.retained_message_bytes = store
                .retained_message_bytes
                .saturating_sub(messages_retained_bytes(&session.messages));
        }
    }

    /// Return the number of active sessions.
    pub async fn session_count(&self) -> usize {
        self.store.read().await.sessions.len()
    }

    /// Check whether a session should be reset and perform reset if needed.
    /// Called periodically or before each interaction.
    pub async fn check_and_reset_if_needed(&self, session_id: &str) -> bool {
        let mut store = self.store.write().await;
        let Some(old_bytes) = store
            .sessions
            .get(session_id)
            .filter(|session| session.should_reset())
            .map(|session| messages_retained_bytes(&session.messages))
        else {
            return false;
        };
        let session = store
            .sessions
            .get_mut(session_id)
            .expect("existing session disappeared under write lock");
        session.messages = Vec::new();
        session.created_at = Utc::now();
        session.last_active_at = Utc::now();
        store.retained_message_bytes = store.retained_message_bytes.saturating_sub(old_bytes);
        true
    }

    /// Build a `SessionContext` for the given session, populating it with
    /// metadata from the session's state.
    pub async fn build_session_context(&self, session_id: &str) -> Option<SessionContext> {
        let store = self.store.read().await;
        store.sessions.get(session_id).map(|session| {
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
        let mut store = self.store.write().await;
        let all: Vec<Session> = store.sessions.values().cloned().collect();
        store.sessions.clear();
        store.retained_message_bytes = 0;
        all
    }

    /// Return all current session snapshots with their canonical keys.
    pub async fn all_sessions(&self) -> Vec<(String, Session)> {
        let store = self.store.read().await;
        store
            .sessions
            .iter()
            .map(|(key, session)| (key.clone(), session.clone()))
            .collect()
    }

    /// Return canonical keys for sessions currently eligible for idle expiry.
    pub async fn idle_session_keys(&self) -> Vec<String> {
        self.store
            .read()
            .await
            .sessions
            .iter()
            .filter(|(_, session)| session.should_reset())
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Remove one session only if it is still eligible for idle expiry.
    pub async fn expire_idle_session_snapshot(&self, session_key: &str) -> Option<Session> {
        let mut store = self.store.write().await;
        if !store
            .sessions
            .get(session_key)
            .is_some_and(Session::should_reset)
        {
            return None;
        }
        let session = store.sessions.remove(session_key)?;
        store.retained_message_bytes = store
            .retained_message_bytes
            .saturating_sub(messages_retained_bytes(&session.messages));
        Some(session)
    }

    /// Expire idle sessions according to their reset policy.
    ///
    /// Returns the removed session snapshots with their canonical keys.
    pub async fn expire_idle_session_snapshots(&self) -> Vec<(String, Session)> {
        let mut expired = Vec::new();
        for session_key in self.idle_session_keys().await {
            if let Some(session) = self.expire_idle_session_snapshot(&session_key).await {
                expired.push((session_key, session));
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
}

fn normalize_session_title(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| title.chars().take(MAX_SESSION_TITLE_CHARS).collect())
}
fn normalize_session_search_text(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn session_matches_query(
    session: &Session,
    key: &str,
    needle: &str,
    normalized_needle: &str,
) -> bool {
    let mut haystacks = vec![
        session.id.as_str(),
        session.platform.as_str(),
        session.chat_id.as_str(),
        key,
    ];
    if let Some(thread_id) = &session.thread_id {
        haystacks.push(thread_id.as_str());
    }
    if let Some(title) = &session.title {
        haystacks.push(title.as_str());
    }

    haystacks.iter().any(|haystack| {
        let lower = haystack.to_ascii_lowercase();
        lower.contains(needle)
            || (!normalized_needle.is_empty()
                && normalize_session_search_text(haystack).contains(normalized_needle))
    })
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
    async fn explicit_scope_isolates_numeric_group_users_and_sibling_threads() {
        let manager = SessionManager::with_resource_limits(SessionConfig::default(), true);
        let thread_a = ConversationScope::new(
            "discord",
            "123456789012345678",
            Some("thread-a"),
            "human-a",
            false,
        );
        let thread_b = ConversationScope::new(
            "discord",
            "123456789012345678",
            Some("thread-b"),
            "human-a",
            false,
        );
        let other_user = ConversationScope::new(
            "discord",
            "123456789012345678",
            Some("thread-a"),
            "human-b",
            false,
        );
        let parent_group =
            ConversationScope::new("discord", "123456789012345678", None, "human-a", false);

        let thread_a_key = manager.compose_session_key_for_scope(thread_a);
        let thread_b_key = manager.compose_session_key_for_scope(thread_b);
        let other_user_key = manager.compose_session_key_for_scope(other_user);
        let parent_group_key = manager.compose_session_key_for_scope(parent_group);
        assert_ne!(thread_a_key, thread_b_key);
        assert_ne!(thread_a_key, other_user_key);
        assert_ne!(thread_a_key, parent_group_key);

        let session = manager.get_or_create_session_for_scope(thread_a).await;
        assert_eq!(session.session_type, SessionType::Thread);
        assert_eq!(session.chat_id, "123456789012345678");
        assert_eq!(session.thread_id.as_deref(), Some("thread-a"));
        assert_eq!(session.user_id, "human-a");
        assert_eq!(
            manager
                .get_or_create_session_for_scope(thread_b)
                .await
                .session_type,
            SessionType::Thread
        );
        assert_eq!(
            manager
                .get_or_create_session_for_scope(parent_group)
                .await
                .session_type,
            SessionType::Group
        );
    }

    #[tokio::test]
    async fn session_manager_add_and_get_messages() {
        let config = SessionConfig::default();
        let manager = SessionManager::new(config);
        let session = manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        let sid =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);

        manager.add_message(&sid, Message::user("hello")).await;
        manager
            .add_message(&sid, Message::assistant("hi there"))
            .await;

        let msgs = manager.get_messages(&sid).await;
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn batched_history_removal_preserves_non_user_undo_predecessors() {
        let manager = SessionManager::new(SessionConfig::default());
        let session = manager
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        let sid =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);

        assert!(
            manager
                .replace_messages(
                    &sid,
                    vec![Message::system("policy"), Message::assistant("answer")],
                )
                .await
        );
        assert_eq!(manager.undo_last_exchange(&sid).await, 1);
        assert_eq!(
            manager.get_messages(&sid).await,
            vec![Message::system("policy")]
        );

        assert!(
            manager
                .replace_messages(
                    &sid,
                    vec![
                        Message::system("policy"),
                        Message::user("question"),
                        Message::assistant("answer"),
                    ],
                )
                .await
        );
        assert_eq!(manager.undo_last_exchange(&sid).await, 2);
        assert_eq!(
            manager.get_messages(&sid).await,
            vec![Message::system("policy")]
        );

        assert!(
            manager
                .replace_messages(
                    &sid,
                    vec![
                        Message::system("policy"),
                        Message::user("one"),
                        Message::assistant("two"),
                        Message::user("three"),
                    ],
                )
                .await
        );
        assert_eq!(manager.remove_last_messages(&sid, 3).await, 3);
        assert_eq!(
            manager.get_messages(&sid).await,
            vec![Message::system("policy")]
        );
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
        let sid =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);

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
        let sid =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);

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
    async fn session_manager_searches_titles_ids_and_keys_without_cross_user_leakage() {
        let manager = SessionManager::new(SessionConfig::default());
        let target = manager
            .get_or_create_session("telegram", "chat-target", "user1")
            .await;
        let target_key =
            manager.compose_session_key(&target.platform, &target.chat_id, &target.user_id);
        manager
            .set_title(&target_key, "AN-94 Prestige Barrel Build #2")
            .await;
        manager
            .get_or_create_session("telegram", "chat-other", "user2")
            .await;
        let other_key = manager.compose_session_key("telegram", "chat-other", "user2");
        manager.set_title(&other_key, "AN-94 other user").await;

        let by_punctuation_folded_title = manager.search_user_sessions("user1", "an94", 10).await;
        assert_eq!(by_punctuation_folded_title.len(), 1);
        assert_eq!(by_punctuation_folded_title[0].chat_id, "chat-target");

        let by_id = manager.search_user_sessions("user1", &target.id, 10).await;
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].id, target.id);

        let by_key = manager.search_user_sessions("user1", &target_key, 10).await;
        assert_eq!(by_key.len(), 1);
        assert_eq!(by_key[0].chat_id, "chat-target");

        assert!(manager
            .search_user_sessions("user1", "other user", 10)
            .await
            .is_empty());
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
    fn legacy_direct_key_delegates_to_explicit_scope_without_id_heuristics() {
        let manager = SessionManager::new(SessionConfig::default());
        let direct = manager.compose_session_key("slack", "C123ABC", "U999");
        let scoped = manager.compose_session_key_for_scope(ConversationScope::new(
            "slack", "C123ABC", None, "U999", true,
        ));
        assert_eq!(direct, scoped);
        assert_ne!(
            direct,
            manager.compose_session_key_for_scope(ConversationScope::new(
                "slack", "C123ABC", None, "U998", true,
            ))
        );
    }
    #[tokio::test]
    async fn implicit_message_limits_are_opt_in_but_explicit_limit_is_always_honored() {
        let unlimited = SessionManager::new(SessionConfig::default());
        let session = unlimited
            .get_or_create_session("test", "unlimited", "user")
            .await;
        let key =
            unlimited.compose_session_key(&session.platform, &session.chat_id, &session.user_id);
        let messages = (0..=DEFAULT_MAX_CONTEXT_MESSAGES)
            .map(|index| Message::assistant(format!("message-{index}")))
            .collect::<Vec<_>>();
        assert!(unlimited.replace_messages(&key, messages).await);
        assert_eq!(
            unlimited.get_messages(&key).await.len(),
            DEFAULT_MAX_CONTEXT_MESSAGES + 1
        );

        let limited = SessionManager::with_resource_limits(SessionConfig::default(), true);
        let session = limited
            .get_or_create_session("test", "limited", "user")
            .await;
        let key =
            limited.compose_session_key(&session.platform, &session.chat_id, &session.user_id);
        let messages = (0..=DEFAULT_MAX_CONTEXT_MESSAGES)
            .map(|index| Message::assistant(format!("message-{index}")))
            .collect::<Vec<_>>();
        assert!(limited.replace_messages(&key, messages).await);
        assert_eq!(
            limited.get_messages(&key).await.len(),
            DEFAULT_MAX_CONTEXT_MESSAGES
        );

        let explicit = SessionManager::new(SessionConfig {
            max_context_messages: Some(2),
            ..SessionConfig::default()
        });
        let session = explicit
            .get_or_create_session("test", "explicit", "user")
            .await;
        let key =
            explicit.compose_session_key(&session.platform, &session.chat_id, &session.user_id);
        assert!(
            explicit
                .replace_messages(
                    &key,
                    vec![
                        Message::assistant("one"),
                        Message::assistant("two"),
                        Message::assistant("three"),
                    ],
                )
                .await
        );
        assert_eq!(explicit.get_messages(&key).await.len(), 2);
    }

    #[tokio::test]
    async fn user_message_admission_fails_without_dropping_the_current_turn() {
        let manager = SessionManager::with_resource_limits(SessionConfig::default(), true);
        let session = manager
            .get_or_create_session("test", "capacity", "user")
            .await;
        let key =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);
        manager.add_message(&key, Message::user("prior turn")).await;
        manager
            .add_message(&key, Message::assistant("prior response"))
            .await;
        let before_messages = manager.get_messages(&key).await;
        let before_bytes = manager.store.read().await.retained_message_bytes;

        assert!(
            !manager
                .try_add_user_message(&key, Message::user("x".repeat(HARD_MAX_CONTEXT_BYTES)),)
                .await
        );
        assert_eq!(manager.get_messages(&key).await, before_messages);
        assert_eq!(
            manager.store.read().await.retained_message_bytes,
            before_bytes
        );
    }

    #[tokio::test]
    async fn explicit_message_limit_trimming_retains_the_newest_user_turn() {
        let manager = SessionManager::new(SessionConfig {
            max_context_messages: Some(2),
            ..SessionConfig::default()
        });
        let session = manager.get_or_create_session("test", "trim", "user").await;
        let key =
            manager.compose_session_key(&session.platform, &session.chat_id, &session.user_id);
        manager.add_message(&key, Message::user("old turn")).await;
        manager
            .add_message(&key, Message::assistant("old response"))
            .await;

        assert!(
            manager
                .try_add_user_message(&key, Message::user("newest turn"))
                .await
        );
        let messages = manager.get_messages(&key).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("newest turn"));
    }

    #[tokio::test]
    async fn bounded_session_creation_keeps_existing_sessions_addressable() {
        let manager = SessionManager::new(SessionConfig::default());
        let first = manager
            .try_get_or_create_session("test", "first", "user", 1)
            .await
            .expect("first session should fit");
        assert!(manager
            .try_get_or_create_session("test", "second", "user", 1)
            .await
            .is_none());
        assert_eq!(
            manager
                .try_get_or_create_session("test", "first", "user", 1)
                .await
                .expect("existing session remains available")
                .id,
            first.id
        );
    }
}
