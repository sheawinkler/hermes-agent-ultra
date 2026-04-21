//! DM (Direct Message) pairing mechanism (Requirement 7.9).
//!
//! Handles authorization decisions when an unregistered user sends a
//! direct message to the bot. Supports configurable behaviors:
//! - Pair: Create a session and request admin approval
//! - Ignore: Silently discard the message

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use hermes_config::UnauthorizedDmBehavior;

// ---------------------------------------------------------------------------
// DmDecision
// ---------------------------------------------------------------------------

/// Decision outcome for a DM from an unregistered/unauthorized user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DmDecision {
    /// Allow the DM through (user is authorized).
    Allow,
    /// Pair the user: create a session and request admin approval.
    Pair {
        /// A message to show the user while awaiting approval.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Deny the DM entirely.
    Deny,
}

// ---------------------------------------------------------------------------
// DmManager
// ---------------------------------------------------------------------------

/// Manages DM authorization decisions for incoming messages.
pub struct DmManager {
    /// Set of user IDs that are explicitly authorized to DM the bot.
    authorized_users: HashSet<String>,

    /// Set of user IDs that have admin privileges.
    admin_users: HashSet<String>,

    /// How to handle DMs from unauthorized users.
    unauthorized_dm_behavior: UnauthorizedDmBehavior,
}

impl DmManager {
    /// Create a new `DmManager`.
    pub fn new(
        authorized_users: HashSet<String>,
        admin_users: HashSet<String>,
        unauthorized_dm_behavior: UnauthorizedDmBehavior,
    ) -> Self {
        Self {
            authorized_users,
            admin_users,
            unauthorized_dm_behavior,
        }
    }

    /// Create a `DmManager` with the Pair behavior and no pre-authorized users.
    pub fn with_pair_behavior() -> Self {
        Self {
            authorized_users: HashSet::new(),
            admin_users: HashSet::new(),
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Pair,
        }
    }

    /// Create a `DmManager` with the Ignore behavior and no pre-authorized users.
    pub fn with_ignore_behavior() -> Self {
        Self {
            authorized_users: HashSet::new(),
            admin_users: HashSet::new(),
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Ignore,
        }
    }

    /// Handle an incoming DM from a user on a platform.
    ///
    /// Returns a `DmDecision` indicating how to proceed:
    /// - `Allow` if the user is authorized or is an admin
    /// - `Pair` if unauthorized and behavior is Pair
    /// - `Deny` if unauthorized and behavior is Ignore
    pub async fn handle_dm(&self, user_id: &str, _platform: &str) -> DmDecision {
        // Admins are always allowed
        if self.admin_users.contains(user_id) {
            return DmDecision::Allow;
        }

        // Authorized users are always allowed
        if self.authorized_users.contains(user_id) {
            return DmDecision::Allow;
        }

        // Unauthorized user: apply the configured behavior
        match self.unauthorized_dm_behavior {
            UnauthorizedDmBehavior::Pair => DmDecision::Pair {
                message: Some(
                    "Your request has been submitted for approval. You will be notified once an admin reviews it.".to_string(),
                ),
            },
            UnauthorizedDmBehavior::Ignore => DmDecision::Deny,
        }
    }

    /// Add a user to the authorized users set.
    pub fn authorize_user(&mut self, user_id: impl Into<String>) {
        self.authorized_users.insert(user_id.into());
    }

    /// Remove a user from the authorized users set.
    pub fn deauthorize_user(&mut self, user_id: &str) {
        self.authorized_users.remove(user_id);
    }

    /// Add a user to the admin users set.
    pub fn add_admin(&mut self, user_id: impl Into<String>) {
        self.admin_users.insert(user_id.into());
    }

    /// Remove a user from the admin users set.
    pub fn remove_admin(&mut self, user_id: &str) {
        self.admin_users.remove(user_id);
    }

    /// Check if a user is authorized.
    pub fn is_authorized(&self, user_id: &str) -> bool {
        self.authorized_users.contains(user_id) || self.admin_users.contains(user_id)
    }

    /// Check if a user is an admin.
    pub fn is_admin(&self, user_id: &str) -> bool {
        self.admin_users.contains(user_id)
    }

    /// Get the number of authorized users.
    pub fn authorized_user_count(&self) -> usize {
        self.authorized_users.len()
    }

    /// Get the number of admin users.
    pub fn admin_user_count(&self) -> usize {
        self.admin_users.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dm_manager_allows_authorized_user() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.authorize_user("user1");

        let decision = dm.handle_dm("user1", "telegram").await;
        assert_eq!(decision, DmDecision::Allow);
    }

    #[tokio::test]
    async fn dm_manager_allows_admin() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.add_admin("admin1");

        let decision = dm.handle_dm("admin1", "discord").await;
        assert_eq!(decision, DmDecision::Allow);
    }

    #[tokio::test]
    async fn dm_manager_pair_behavior() {
        let dm = DmManager::with_pair_behavior();
        let decision = dm.handle_dm("unknown_user", "telegram").await;
        assert!(matches!(decision, DmDecision::Pair { .. }));
    }

    #[tokio::test]
    async fn dm_manager_ignore_behavior() {
        let dm = DmManager::with_ignore_behavior();
        let decision = dm.handle_dm("unknown_user", "telegram").await;
        assert_eq!(decision, DmDecision::Deny);
    }

    #[tokio::test]
    async fn dm_manager_authorize_and_deauthorize() {
        let mut dm = DmManager::with_ignore_behavior();
        dm.authorize_user("user1");
        assert!(dm.is_authorized("user1"));

        dm.deauthorize_user("user1");
        assert!(!dm.is_authorized("user1"));
    }

    #[test]
    fn dm_decision_serde() {
        let allow = DmDecision::Allow;
        let json = serde_json::to_string(&allow).unwrap();
        assert!(json.contains("allow"));

        let pair = DmDecision::Pair {
            message: Some("pending".to_string()),
        };
        let json = serde_json::to_string(&pair).unwrap();
        assert!(json.contains("pair"));

        let deny = DmDecision::Deny;
        let json = serde_json::to_string(&deny).unwrap();
        assert!(json.contains("deny"));
    }
}
