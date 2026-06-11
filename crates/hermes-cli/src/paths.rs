//! Centralized well-known paths under a Hermes CLI state root.

use std::path::{Path, PathBuf};

use hermes_config::{gateway_pid_path_in, state_dir};

/// Newtype for the resolved Hermes state root (`HERMES_HOME` / `-C`).
#[derive(Debug, Clone)]
pub struct CliStateRoot {
    root: PathBuf,
}

impl CliStateRoot {
    pub fn from_config_dir(config_dir: Option<&Path>) -> Self {
        Self {
            root: state_dir(config_dir),
        }
    }

    pub fn from_path(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_yaml(&self) -> PathBuf {
        self.root.join("config.yaml")
    }

    pub fn gateway_pid(&self) -> PathBuf {
        gateway_pid_path_in(&self.root)
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn cron_dir(&self) -> PathBuf {
        self.root.join("cron")
    }

    pub fn webhooks_json(&self) -> PathBuf {
        self.root.join("webhooks.json")
    }

    pub fn webhook_subscriptions_json(&self) -> PathBuf {
        self.root.join("webhook_subscriptions.json")
    }

    pub fn auth_pool(&self) -> PathBuf {
        self.root.join("auth").join("pool.json")
    }

    pub fn secret_vault(&self) -> PathBuf {
        self.root.join("auth").join("tokens.json")
    }

    pub fn provenance_key(&self) -> PathBuf {
        self.root.join("auth").join("provenance.key")
    }

    pub fn route_learning_state(&self) -> PathBuf {
        self.root.join("logs").join("route-learning.json")
    }

    pub fn route_health_state(&self) -> PathBuf {
        self.root.join("logs").join("route-health.json")
    }

    pub fn route_autotune_state(&self) -> PathBuf {
        self.root.join("logs").join("route-autotune.json")
    }
}
