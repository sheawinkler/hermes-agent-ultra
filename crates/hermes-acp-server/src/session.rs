//! Lightweight per-connection session state.
//!
//! Does NOT use hermes-acp::SessionManager (which is designed for stdin + persistence).
//! Each ACP connection gets its own PipeSession instance.

use serde_json::Value;
use std::fmt;
use std::time::Instant;

/// Per-connection ACP session.
#[derive(Clone)]
pub struct PipeSession {
    /// Session ID (e.g. "acp:main:<uuid>").
    pub session_id: String,
    /// Working directory provided by the client in session/new.
    pub cwd: Option<String>,
    /// Current Skill/mode set via session/set_mode.
    pub mode: Option<String>,
    /// Client name from initialize (e.g. "ai-cherry").
    pub client_name: Option<String>,
    /// Client title from initialize (e.g. "AI_Cherry").
    pub client_title: Option<String>,
    /// Client version from initialize.
    pub client_version: Option<String>,
    /// Conversation history (assistant + user messages).
    pub history: Vec<Value>,
    /// When this session was created.
    pub created_at: Instant,
    /// Source from session/new _meta (e.g. "asr", "text", "weixin").
    pub source: Option<String>,
    /// Channel from session/new _meta (e.g. "desktop-pet", "weixin").
    pub channel: Option<String>,
    /// Active skill ID from session/new or session/set_mode.
    pub skill_id: Option<String>,
}

impl PipeSession {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            cwd: None,
            mode: None,
            client_name: None,
            client_title: None,
            client_version: None,
            history: Vec::new(),
            created_at: Instant::now(),
            source: None,
            channel: None,
            skill_id: None,
        }
    }
}

impl fmt::Debug for PipeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipeSession")
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("mode", &self.mode)
            .field("client_name", &self.client_name)
            .field("client_title", &self.client_title)
            .field("client_version", &self.client_version)
            .field("history_len", &self.history.len())
            .field("source", &self.source)
            .field("channel", &self.channel)
            .field("skill_id", &self.skill_id)
            .finish_non_exhaustive()
    }
}

/// Metadata update pushed from a connection back to the server-level map.
/// Used by the `ConnectionMetaCb` callback so each field is named explicitly.
#[derive(Debug, Clone, Default)]
pub struct MetaUpdate {
    pub client_name: Option<String>,
    pub session_id: Option<String>,
    pub client_title: Option<String>,
}
