//! ACP permission bridging — maps ACP approval requests to Hermes approval
//! callbacks.
//!
//! Mirrors the Python `acp_adapter/permissions.py` implementation.

use std::fmt;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Permission types
// ---------------------------------------------------------------------------

/// An individual permission option presented to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOption {
    pub option_id: String,
    pub kind: PermissionKind,
    pub name: String,
}

/// Permission decision kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermissionKind {
    /// Map ACP permission kind to a Hermes approval result string.
    pub fn to_hermes_result(self) -> &'static str {
        match self {
            Self::AllowOnce => "once",
            Self::AllowAlways => "always",
            Self::RejectOnce => "deny",
            Self::RejectAlways => "deny",
        }
    }
}

impl fmt::Display for PermissionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllowOnce => write!(f, "allow_once"),
            Self::AllowAlways => write!(f, "allow_always"),
            Self::RejectOnce => write!(f, "reject_once"),
            Self::RejectAlways => write!(f, "reject_always"),
        }
    }
}

/// A pending permission request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub session_id: String,
    pub command: String,
    pub description: String,
    pub options: Vec<PermissionOption>,
    pub created_at: u64,
}

/// The outcome of a permission decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionOutcome {
    Allowed { option_id: String },
    Denied,
    Timeout,
}

// ---------------------------------------------------------------------------
// Default permission options
// ---------------------------------------------------------------------------

/// Build the standard set of permission options.
pub fn default_permission_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            option_id: "allow_once".to_string(),
            kind: PermissionKind::AllowOnce,
            name: "Allow once".to_string(),
        },
        PermissionOption {
            option_id: "allow_always".to_string(),
            kind: PermissionKind::AllowAlways,
            name: "Allow always".to_string(),
        },
        PermissionOption {
            option_id: "deny".to_string(),
            kind: PermissionKind::RejectOnce,
            name: "Deny".to_string(),
        },
    ]
}

// ---------------------------------------------------------------------------
// ApprovalCallback
// ---------------------------------------------------------------------------

/// A synchronous approval callback that can be used by the agent's tool
/// execution layer to gate dangerous operations.
///
/// When the ACP client is connected, this bridges to the client's
/// `request_permission` flow. When no client is connected, it auto-denies.
pub type ApprovalCallback = Arc<dyn Fn(&str, &str) -> String + Send + Sync>;

/// Create an auto-deny approval callback (for when no ACP client is connected).
pub fn auto_deny_callback() -> ApprovalCallback {
    Arc::new(|_command, _description| "deny".to_string())
}

/// Create an auto-allow approval callback (for trusted environments).
pub fn auto_allow_callback() -> ApprovalCallback {
    Arc::new(|_command, _description| "always".to_string())
}

// ---------------------------------------------------------------------------
// PermissionStore
// ---------------------------------------------------------------------------

/// In-memory store for pending permission requests.
/// Used by the MCP serve bridge to track and resolve approvals.
pub struct PermissionStore {
    pending: Mutex<Vec<PermissionRequest>>,
    resolved: Mutex<Vec<(String, PermissionOutcome)>>,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            resolved: Mutex::new(Vec::new()),
        }
    }

    /// Add a new pending permission request.
    pub fn add_pending(&self, request: PermissionRequest) {
        self.pending.lock().unwrap().push(request);
    }

    /// List all pending permission requests.
    pub fn list_pending(&self) -> Vec<PermissionRequest> {
        self.pending.lock().unwrap().clone()
    }

    /// Resolve a pending permission request by ID.
    pub fn resolve(&self, request_id: &str, outcome: PermissionOutcome) -> bool {
        let mut pending = self.pending.lock().unwrap();
        let idx = pending.iter().position(|r| r.id == request_id);
        if let Some(idx) = idx {
            pending.remove(idx);
            self.resolved
                .lock()
                .unwrap()
                .push((request_id.to_string(), outcome));
            true
        } else {
            false
        }
    }

    /// Get resolved outcomes (drain).
    pub fn drain_resolved(&self) -> Vec<(String, PermissionOutcome)> {
        let mut resolved = self.resolved.lock().unwrap();
        std::mem::take(&mut *resolved)
    }
}

impl Default for PermissionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_kind_to_hermes() {
        assert_eq!(PermissionKind::AllowOnce.to_hermes_result(), "once");
        assert_eq!(PermissionKind::AllowAlways.to_hermes_result(), "always");
        assert_eq!(PermissionKind::RejectOnce.to_hermes_result(), "deny");
    }

    #[test]
    fn test_permission_store() {
        let store = PermissionStore::new();
        store.add_pending(PermissionRequest {
            id: "req1".to_string(),
            session_id: "s1".to_string(),
            command: "rm -rf /".to_string(),
            description: "dangerous command".to_string(),
            options: default_permission_options(),
            created_at: 0,
        });

        assert_eq!(store.list_pending().len(), 1);
        assert!(store.resolve("req1", PermissionOutcome::Denied));
        assert!(store.list_pending().is_empty());
        assert_eq!(store.drain_resolved().len(), 1);
    }

    #[test]
    fn test_auto_callbacks() {
        let deny = auto_deny_callback();
        assert_eq!(deny("cmd", "desc"), "deny");

        let allow = auto_allow_callback();
        assert_eq!(allow("cmd", "desc"), "always");
    }
}
