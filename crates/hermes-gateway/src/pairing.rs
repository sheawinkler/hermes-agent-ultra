//! DM pairing workflow helpers.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingState {
    Pending,
    Approved,
    Denied,
}

#[derive(Clone, Default)]
pub struct PairingManager {
    sessions: Arc<RwLock<HashMap<String, PairingState>>>,
}

impl PairingManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self, user_id: impl Into<String>) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(user_id.into(), PairingState::Pending);
        }
    }

    pub fn approve(&self, user_id: &str) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(user_id.to_string(), PairingState::Approved);
        }
    }

    pub fn deny(&self, user_id: &str) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(user_id.to_string(), PairingState::Denied);
        }
    }

    pub fn state(&self, user_id: &str) -> Option<PairingState> {
        self.sessions.read().ok().and_then(|s| s.get(user_id).cloned())
    }
}
