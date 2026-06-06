//! ACP permission bridging — maps ACP approval requests to Hermes approval
//! callbacks.
//!
//! Mirrors the Python `acp_adapter/permissions.py` implementation.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
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

impl PermissionOutcome {
    /// Map an ACP permission outcome into Hermes approval strings.
    ///
    /// ACP "kind" alone cannot express Hermes' session-scoped allowance, so
    /// mapping is intentionally based on the stable option id.
    pub fn to_hermes_result(&self, allowed_options: &[PermissionOption]) -> &'static str {
        let Self::Allowed { option_id } = self else {
            return "deny";
        };
        if !allowed_options
            .iter()
            .any(|option| option.option_id == *option_id)
        {
            return "deny";
        }
        match option_id.as_str() {
            "allow_once" => "once",
            "allow_session" => "session",
            "allow_always" => "always",
            "deny" | "deny_always" => "deny",
            _ => "deny",
        }
    }
}

// ---------------------------------------------------------------------------
// Default permission options
// ---------------------------------------------------------------------------

/// Build the standard set of permission options.
pub fn default_permission_options() -> Vec<PermissionOption> {
    permission_options(true)
}

/// Build ACP permission options that match Hermes approval semantics.
pub fn permission_options(allow_permanent: bool) -> Vec<PermissionOption> {
    let mut options = vec![
        PermissionOption {
            option_id: "allow_once".to_string(),
            kind: PermissionKind::AllowOnce,
            name: "Allow once".to_string(),
        },
        PermissionOption {
            option_id: "allow_session".to_string(),
            // ACP has no session-scoped kind; keep Hermes semantics in the
            // stable option id while using the closest ACP persistence hint.
            kind: PermissionKind::AllowAlways,
            name: "Allow for session".to_string(),
        },
    ];
    if allow_permanent {
        options.push(PermissionOption {
            option_id: "allow_always".to_string(),
            kind: PermissionKind::AllowAlways,
            name: "Allow always".to_string(),
        });
    }
    options.extend([
        PermissionOption {
            option_id: "deny".to_string(),
            kind: PermissionKind::RejectOnce,
            name: "Deny".to_string(),
        },
        PermissionOption {
            option_id: "deny_always".to_string(),
            kind: PermissionKind::RejectAlways,
            name: "Deny always".to_string(),
        },
    ]);
    options
}

static PERMISSION_REQUEST_IDS: AtomicU64 = AtomicU64::new(1);

/// Build a pending permission request with a stable, unique `perm-check-N` id.
pub fn build_permission_request(
    session_id: impl Into<String>,
    command: impl Into<String>,
    description: impl Into<String>,
    allow_permanent: bool,
    created_at: u64,
) -> PermissionRequest {
    let id = PERMISSION_REQUEST_IDS.fetch_add(1, Ordering::Relaxed);
    PermissionRequest {
        id: format!("perm-check-{id}"),
        session_id: session_id.into(),
        command: command.into(),
        description: description.into(),
        options: permission_options(allow_permanent),
        created_at,
    }
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
        assert_eq!(PermissionKind::RejectAlways.to_hermes_result(), "deny");
    }

    #[test]
    fn test_permission_options_match_upstream_contract() {
        let options = permission_options(true);
        let ids: Vec<_> = options
            .iter()
            .map(|option| (option.option_id.as_str(), option.kind, option.name.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("allow_once", PermissionKind::AllowOnce, "Allow once"),
                (
                    "allow_session",
                    PermissionKind::AllowAlways,
                    "Allow for session"
                ),
                ("allow_always", PermissionKind::AllowAlways, "Allow always"),
                ("deny", PermissionKind::RejectOnce, "Deny"),
                ("deny_always", PermissionKind::RejectAlways, "Deny always"),
            ]
        );

        let no_permanent_options = permission_options(false);
        let no_permanent_ids: Vec<_> = no_permanent_options
            .iter()
            .map(|option| option.option_id.as_str())
            .collect();
        assert_eq!(
            no_permanent_ids,
            vec!["allow_once", "allow_session", "deny", "deny_always"]
        );
    }

    #[test]
    fn test_permission_outcome_maps_by_option_id() {
        let options = permission_options(true);
        for (option_id, expected) in [
            ("allow_once", "once"),
            ("allow_session", "session"),
            ("allow_always", "always"),
            ("deny", "deny"),
            ("deny_always", "deny"),
            ("unexpected", "deny"),
        ] {
            let outcome = PermissionOutcome::Allowed {
                option_id: option_id.to_string(),
            };
            assert_eq!(outcome.to_hermes_result(&options), expected);
        }
        assert_eq!(PermissionOutcome::Denied.to_hermes_result(&options), "deny");
        assert_eq!(
            PermissionOutcome::Timeout.to_hermes_result(&options),
            "deny"
        );
    }

    #[test]
    fn test_permission_request_builder_uses_unique_perm_check_ids() {
        let first = build_permission_request("s1", "rm -rf /", "dangerous command", true, 42);
        let second = build_permission_request("s1", "echo ok", "safe command", false, 43);

        assert!(first.id.starts_with("perm-check-"));
        assert!(second.id.starts_with("perm-check-"));
        assert_ne!(first.id, second.id);
        assert_eq!(first.session_id, "s1");
        assert_eq!(first.command, "rm -rf /");
        assert_eq!(first.description, "dangerous command");
        assert_eq!(first.created_at, 42);
        assert_eq!(
            second
                .options
                .iter()
                .map(|option| option.option_id.as_str())
                .collect::<Vec<_>>(),
            vec!["allow_once", "allow_session", "deny", "deny_always"]
        );
    }

    #[test]
    fn test_permission_store() {
        let store = PermissionStore::new();
        let mut request = build_permission_request("s1", "rm -rf /", "dangerous command", true, 0);
        request.id = "req1".to_string();
        store.add_pending(request);

        assert_eq!(store.list_pending().len(), 1);
        assert!(store.resolve("req1", PermissionOutcome::Denied));
        assert!(store.list_pending().is_empty());
        assert_eq!(store.drain_resolved().len(), 1);
    }

    #[test]
    fn permission_store_instances_do_not_share_pending_or_resolved_requests() {
        let store_a = PermissionStore::new();
        let store_b = PermissionStore::new();
        let mut request = build_permission_request(
            "acp-session-A",
            "rm -rf /tmp/a",
            "dangerous command",
            true,
            0,
        );
        request.id = "req-a".to_string();

        store_a.add_pending(request);
        assert_eq!(store_a.list_pending().len(), 1);
        assert!(store_b.list_pending().is_empty());

        assert!(store_a.resolve("req-a", PermissionOutcome::Denied));
        assert!(!store_b.resolve("req-a", PermissionOutcome::Denied));
        assert_eq!(store_a.drain_resolved().len(), 1);
        assert!(store_b.drain_resolved().is_empty());
    }

    #[test]
    fn permission_store_keeps_overlapping_sessions_isolated() {
        use std::sync::{Arc, Barrier};

        let store = Arc::new(PermissionStore::new());
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();

        for (session_id, request_id, command, outcome) in [
            (
                "acp-session-A",
                "req-a",
                "rm -rf /tmp/a",
                PermissionOutcome::Denied,
            ),
            (
                "acp-session-B",
                "req-b",
                "sudo apt update",
                PermissionOutcome::Allowed {
                    option_id: "allow_once".to_string(),
                },
            ),
        ] {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut request =
                    build_permission_request(session_id, command, "dangerous command", true, 0);
                request.id = request_id.to_string();
                store.add_pending(request);
                barrier.wait();
                assert!(store.resolve(request_id, outcome));
            }));
        }

        barrier.wait();
        for handle in handles {
            handle.join().expect("permission thread should not panic");
        }

        assert!(store.list_pending().is_empty());
        let mut resolved = store.drain_resolved();
        resolved.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].0, "req-a");
        assert!(matches!(resolved[0].1, PermissionOutcome::Denied));
        assert_eq!(resolved[1].0, "req-b");
        assert!(matches!(
            &resolved[1].1,
            PermissionOutcome::Allowed { option_id } if option_id == "allow_once"
        ));
    }

    #[test]
    fn test_auto_callbacks() {
        let deny = auto_deny_callback();
        assert_eq!(deny("cmd", "desc"), "deny");

        let allow = auto_allow_callback();
        assert_eq!(allow("cmd", "desc"), "always");
    }
}
