//! Hermes MCP Serve — expose messaging conversations as MCP tools.
//!
//! Mirrors the Python `mcp_serve.py`. Starts a stdio MCP server that lets any
//! MCP client (Claude Code, Cursor, Codex, etc.) list conversations, read
//! message history, send messages, poll for live events, and manage approval
//! requests.
//!
//! Tools exposed:
//!   - `session_list` — list active sessions
//!   - `session_read` — read messages from a session
//!   - `session_send` — send a user message into a session
//!   - `events_poll`  — poll for new events since a cursor
//!   - `events_wait`  — long-poll for next event (blocking)
//!   - `approve`      — approve a pending permission request
//!   - `deny`         — deny a pending permission request

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::McpError;

// ---------------------------------------------------------------------------
// EventBridge — in-memory event queue with cursor-based polling
// ---------------------------------------------------------------------------

const QUEUE_LIMIT: usize = 1000;

/// A single event in the bridge's event queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeEvent {
    pub cursor: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub session_key: String,
    #[serde(flatten)]
    pub data: HashMap<String, Value>,
}

/// In-memory event queue with cursor-based polling and blocking wait support.
///
/// The Rust equivalent of Python's `EventBridge`.
pub struct EventBridge {
    queue: Mutex<Vec<BridgeEvent>>,
    cursor: AtomicU64,
    notify: tokio::sync::Notify,
}

impl EventBridge {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            cursor: AtomicU64::new(0),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Push an event into the queue. Sets `cursor` automatically.
    pub fn push(&self, event_type: &str, session_key: &str, data: HashMap<String, Value>) {
        let cursor = self.cursor.fetch_add(1, Ordering::Relaxed) + 1;
        let event = BridgeEvent {
            cursor,
            event_type: event_type.to_string(),
            session_key: session_key.to_string(),
            data,
        };
        let mut q = self.queue.lock().unwrap();
        q.push(event);
        while q.len() > QUEUE_LIMIT {
            q.remove(0);
        }
        drop(q);
        self.notify.notify_waiters();
    }

    /// Poll for events after `after_cursor`, optionally filtered by `session_key`.
    pub fn poll(
        &self,
        after_cursor: u64,
        session_key: Option<&str>,
        limit: usize,
    ) -> (Vec<BridgeEvent>, u64) {
        let q = self.queue.lock().unwrap();
        let events: Vec<BridgeEvent> = q
            .iter()
            .filter(|e| e.cursor > after_cursor)
            .filter(|e| session_key.map_or(true, |sk| e.session_key == sk))
            .take(limit)
            .cloned()
            .collect();
        let next = events.last().map_or(after_cursor, |e| e.cursor);
        (events, next)
    }

    /// Wait for an event matching the criteria, with a timeout.
    pub async fn wait(
        &self,
        after_cursor: u64,
        session_key: Option<&str>,
        timeout: Duration,
    ) -> Option<BridgeEvent> {
        let deadline = Instant::now() + timeout;

        loop {
            {
                let q = self.queue.lock().unwrap();
                for e in q.iter() {
                    if e.cursor > after_cursor && session_key.map_or(true, |sk| e.session_key == sk)
                    {
                        return Some(e.clone());
                    }
                }
            }

            let remaining = deadline.checked_duration_since(Instant::now())?;
            if remaining.is_zero() {
                return None;
            }

            tokio::select! {
                _ = self.notify.notified() => {},
                _ = tokio::time::sleep(remaining.min(Duration::from_millis(200))) => {},
            }

            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    pub fn current_cursor(&self) -> u64 {
        self.cursor.load(Ordering::Relaxed)
    }
}

impl Default for EventBridge {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Session Store — lightweight read-only accessor for persisted sessions
// ---------------------------------------------------------------------------

/// Minimal session info for MCP listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub session_key: String,
    pub session_id: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub updated_at: String,
}

/// Minimal message info for MCP reading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    #[serde(default)]
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
}

/// Trait for session persistence backends.
///
/// Implementations can read from SQLite, JSON files, or in-memory stores.
pub trait SessionStore: Send + Sync {
    fn list_sessions(&self) -> Vec<SessionEntry>;
    fn get_messages(&self, session_id: &str, limit: usize) -> Vec<SessionMessage>;
}

/// In-memory session store (for testing and embedded use).
pub struct InMemorySessionStore {
    sessions: Mutex<Vec<SessionEntry>>,
    messages: Mutex<HashMap<String, Vec<SessionMessage>>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
            messages: Mutex::new(HashMap::new()),
        }
    }

    pub fn add_session(&self, entry: SessionEntry) {
        self.sessions.lock().unwrap().push(entry);
    }

    pub fn add_message(&self, session_id: &str, msg: SessionMessage) {
        self.messages
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .push(msg);
    }
}

impl Default for InMemorySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore for InMemorySessionStore {
    fn list_sessions(&self) -> Vec<SessionEntry> {
        self.sessions.lock().unwrap().clone()
    }

    fn get_messages(&self, session_id: &str, limit: usize) -> Vec<SessionMessage> {
        let map = self.messages.lock().unwrap();
        match map.get(session_id) {
            Some(msgs) => {
                let start = msgs.len().saturating_sub(limit);
                msgs[start..].to_vec()
            }
            None => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Approval Store
// ---------------------------------------------------------------------------

/// A pending approval request surfaced via MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: String,
    pub session_key: String,
    pub command: String,
    pub description: String,
    pub created_at: String,
}

/// Thread-safe store for pending approvals.
pub struct ApprovalStore {
    pending: Mutex<HashMap<String, PendingApproval>>,
    resolved: Mutex<Vec<(String, String)>>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            resolved: Mutex::new(Vec::new()),
        }
    }

    pub fn add_pending(&self, approval: PendingApproval) {
        self.pending
            .lock()
            .unwrap()
            .insert(approval.id.clone(), approval);
    }

    pub fn list_pending(&self) -> Vec<PendingApproval> {
        self.pending.lock().unwrap().values().cloned().collect()
    }

    pub fn resolve(&self, id: &str, decision: &str) -> Option<PendingApproval> {
        let removed = self.pending.lock().unwrap().remove(id);
        if let Some(ref approval) = removed {
            self.resolved
                .lock()
                .unwrap()
                .push((approval.id.clone(), decision.to_string()));
        }
        removed
    }
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// HermesMcpServe — the MCP server exposing session tools
// ---------------------------------------------------------------------------

/// Hermes MCP Server that exposes session management, event polling,
/// and approval handling as MCP tools.
pub struct HermesMcpServe {
    pub event_bridge: Arc<EventBridge>,
    pub session_store: Arc<dyn SessionStore>,
    pub approval_store: Arc<ApprovalStore>,
    server_version: String,
}

impl HermesMcpServe {
    pub fn new(session_store: Arc<dyn SessionStore>) -> Self {
        Self {
            event_bridge: Arc::new(EventBridge::new()),
            session_store,
            approval_store: Arc::new(ApprovalStore::new()),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn with_event_bridge(mut self, bridge: Arc<EventBridge>) -> Self {
        self.event_bridge = bridge;
        self
    }

    pub fn with_approval_store(mut self, store: Arc<ApprovalStore>) -> Self {
        self.approval_store = store;
        self
    }

    /// Handle an incoming MCP tool call.
    pub async fn handle_tool_call(&self, tool_name: &str, args: Value) -> Result<Value, McpError> {
        match tool_name {
            "session_list" => self.tool_session_list(args),
            "session_read" => self.tool_session_read(args),
            "session_send" => self.tool_session_send(args),
            "events_poll" => self.tool_events_poll(args),
            "events_wait" => self.tool_events_wait(args).await,
            "approve" => self.tool_approve(args),
            "deny" => self.tool_deny(args),
            _ => Err(McpError::MethodNotFound(tool_name.to_string())),
        }
    }

    /// Return tool definitions for MCP `tools/list`.
    pub fn tool_definitions(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "session_list",
                "description": "List active Hermes conversations/sessions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "platform": {"type": "string", "description": "Filter by platform."},
                        "limit": {"type": "integer", "description": "Max results (default 50)."},
                        "search": {"type": "string", "description": "Filter by name."}
                    }
                }
            }),
            json!({
                "name": "session_read",
                "description": "Read recent messages from a session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_key": {"type": "string", "description": "Session key."},
                        "limit": {"type": "integer", "description": "Max messages (default 50)."}
                    },
                    "required": ["session_key"]
                }
            }),
            json!({
                "name": "session_send",
                "description": "Send a message into a session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_key": {"type": "string", "description": "Target session key."},
                        "message": {"type": "string", "description": "Message text."}
                    },
                    "required": ["session_key", "message"]
                }
            }),
            json!({
                "name": "events_poll",
                "description": "Poll for new events since a cursor.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "after_cursor": {"type": "integer", "description": "Cursor position (0 for all)."},
                        "session_key": {"type": "string", "description": "Filter to one session."},
                        "limit": {"type": "integer", "description": "Max events (default 20)."}
                    }
                }
            }),
            json!({
                "name": "events_wait",
                "description": "Long-poll for the next event. Blocks until event or timeout.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "after_cursor": {"type": "integer", "description": "Cursor position."},
                        "session_key": {"type": "string", "description": "Filter to one session."},
                        "timeout_ms": {"type": "integer", "description": "Max wait in ms (default 30000)."}
                    }
                }
            }),
            json!({
                "name": "approve",
                "description": "Approve a pending permission request.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Approval request ID."}
                    },
                    "required": ["id"]
                }
            }),
            json!({
                "name": "deny",
                "description": "Deny a pending permission request.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Approval request ID."}
                    },
                    "required": ["id"]
                }
            }),
        ]
    }

    // -- Tool implementations ---------------------------------------------

    fn tool_session_list(&self, args: Value) -> Result<Value, McpError> {
        let platform = args.get("platform").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let search = args.get("search").and_then(|v| v.as_str());

        let sessions = self.session_store.list_sessions();
        let filtered: Vec<&SessionEntry> = sessions
            .iter()
            .filter(|s| platform.map_or(true, |p| s.platform.eq_ignore_ascii_case(p)))
            .filter(|s| {
                search.map_or(true, |q| {
                    let q = q.to_lowercase();
                    s.display_name.to_lowercase().contains(&q)
                        || s.session_key.to_lowercase().contains(&q)
                })
            })
            .take(limit)
            .collect();

        Ok(json!({
            "count": filtered.len(),
            "sessions": filtered,
        }))
    }

    fn tool_session_read(&self, args: Value) -> Result<Value, McpError> {
        let session_key = args
            .get("session_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing session_key".into()))?;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

        let sessions = self.session_store.list_sessions();
        let entry = sessions
            .iter()
            .find(|s| s.session_key == session_key)
            .ok_or_else(|| McpError::ResourceNotFound(session_key.into()))?;

        let messages = self.session_store.get_messages(&entry.session_id, limit);

        Ok(json!({
            "session_key": session_key,
            "count": messages.len(),
            "messages": messages,
        }))
    }

    fn tool_session_send(&self, args: Value) -> Result<Value, McpError> {
        let session_key = args
            .get("session_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing session_key".into()))?;
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing message".into()))?;

        let mut data = HashMap::new();
        data.insert("role".to_string(), json!("user"));
        data.insert("content".to_string(), json!(message));
        self.event_bridge.push("message", session_key, data);

        Ok(json!({
            "sent": true,
            "session_key": session_key,
        }))
    }

    fn tool_events_poll(&self, args: Value) -> Result<Value, McpError> {
        let after = args
            .get("after_cursor")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let session_key = args.get("session_key").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let (events, next_cursor) = self.event_bridge.poll(after, session_key, limit);

        Ok(json!({
            "events": events,
            "next_cursor": next_cursor,
        }))
    }

    async fn tool_events_wait(&self, args: Value) -> Result<Value, McpError> {
        let after = args
            .get("after_cursor")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let session_key = args.get("session_key").and_then(|v| v.as_str());
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(30_000)
            .min(300_000); // cap at 5 minutes

        let timeout = Duration::from_millis(timeout_ms);
        let event = self.event_bridge.wait(after, session_key, timeout).await;

        match event {
            Some(e) => Ok(json!({"event": e})),
            None => Ok(json!({"event": null, "reason": "timeout"})),
        }
    }

    fn tool_approve(&self, args: Value) -> Result<Value, McpError> {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing id".into()))?;

        match self.approval_store.resolve(id, "allow-once") {
            Some(approval) => {
                let mut data = HashMap::new();
                data.insert("approval_id".to_string(), json!(id));
                data.insert("decision".to_string(), json!("allow-once"));
                self.event_bridge
                    .push("approval_resolved", &approval.session_key, data);
                Ok(json!({"resolved": true, "id": id, "decision": "allow-once"}))
            }
            None => Ok(json!({"error": format!("Approval not found: {}", id)})),
        }
    }

    fn tool_deny(&self, args: Value) -> Result<Value, McpError> {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidParams("missing id".into()))?;

        match self.approval_store.resolve(id, "deny") {
            Some(approval) => {
                let mut data = HashMap::new();
                data.insert("approval_id".to_string(), json!(id));
                data.insert("decision".to_string(), json!("deny"));
                self.event_bridge
                    .push("approval_resolved", &approval.session_key, data);
                Ok(json!({"resolved": true, "id": id, "decision": "deny"}))
            }
            None => Ok(json!({"error": format!("Approval not found: {}", id)})),
        }
    }

    /// Handle a full MCP JSON-RPC request (initialize, tools/list, tools/call).
    pub async fn handle_request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {"listChanged": false},
                },
                "serverInfo": {
                    "name": "hermes-mcp-serve",
                    "version": self.server_version,
                }
            })),
            "tools/list" => {
                let tools = self.tool_definitions();
                Ok(json!({"tools": tools}))
            }
            "tools/call" => {
                let tool_name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| McpError::InvalidParams("missing tool name".into()))?;
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                let result = self.handle_tool_call(tool_name, arguments).await?;
                Ok(json!({
                    "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}],
                    "isError": false,
                }))
            }
            "ping" => Ok(json!({})),
            _ => Err(McpError::MethodNotFound(method.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_bridge_push_poll() {
        let bridge = EventBridge::new();
        bridge.push("message", "s1", HashMap::new());
        bridge.push("message", "s2", HashMap::new());

        let (events, cursor) = bridge.poll(0, None, 10);
        assert_eq!(events.len(), 2);
        assert_eq!(cursor, 2);

        let (events, _) = bridge.poll(0, Some("s1"), 10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_key, "s1");
    }

    #[test]
    fn test_event_bridge_queue_limit() {
        let bridge = EventBridge::new();
        for i in 0..QUEUE_LIMIT + 50 {
            bridge.push("msg", &format!("s{}", i), HashMap::new());
        }
        let (events, _) = bridge.poll(0, None, QUEUE_LIMIT + 100);
        assert_eq!(events.len(), QUEUE_LIMIT);
    }

    #[test]
    fn test_approval_store() {
        let store = ApprovalStore::new();
        store.add_pending(PendingApproval {
            id: "a1".into(),
            session_key: "s1".into(),
            command: "rm -rf".into(),
            description: "Delete files".into(),
            created_at: "2024-01-01".into(),
        });

        assert_eq!(store.list_pending().len(), 1);

        let resolved = store.resolve("a1", "allow-once");
        assert!(resolved.is_some());
        assert_eq!(store.list_pending().len(), 0);

        let not_found = store.resolve("a2", "deny");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_in_memory_session_store() {
        let store = InMemorySessionStore::new();
        store.add_session(SessionEntry {
            session_key: "test".into(),
            session_id: "sid1".into(),
            platform: "cli".into(),
            display_name: "Test Session".into(),
            updated_at: "2024-01-01".into(),
        });
        store.add_message(
            "sid1",
            SessionMessage {
                id: "m1".into(),
                role: "user".into(),
                content: "Hello".into(),
                timestamp: "2024-01-01".into(),
            },
        );

        let sessions = store.list_sessions();
        assert_eq!(sessions.len(), 1);

        let messages = store.get_messages("sid1", 50);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Hello");
    }

    #[tokio::test]
    async fn test_hermes_mcp_serve_tool_defs() {
        let store = Arc::new(InMemorySessionStore::new());
        let serve = HermesMcpServe::new(store);
        let defs = serve.tool_definitions();
        assert_eq!(defs.len(), 7);
    }

    #[tokio::test]
    async fn test_session_list() {
        let store = Arc::new(InMemorySessionStore::new());
        store.add_session(SessionEntry {
            session_key: "k1".into(),
            session_id: "s1".into(),
            platform: "telegram".into(),
            display_name: "Alice".into(),
            updated_at: "".into(),
        });

        let serve = HermesMcpServe::new(store);
        let result = serve
            .handle_tool_call("session_list", json!({}))
            .await
            .unwrap();
        assert_eq!(result["count"], 1);
    }

    #[tokio::test]
    async fn test_events_poll() {
        let store = Arc::new(InMemorySessionStore::new());
        let serve = HermesMcpServe::new(store);

        serve.event_bridge.push("message", "s1", HashMap::new());

        let result = serve
            .handle_tool_call("events_poll", json!({"after_cursor": 0}))
            .await
            .unwrap();
        assert_eq!(result["events"].as_array().unwrap().len(), 1);
    }
}
