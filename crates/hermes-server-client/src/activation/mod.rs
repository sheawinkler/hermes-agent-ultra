//! Device activation reporting (`POST /device/activate`).

mod fingerprint;

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

pub use fingerprint::{DeviceFingerprint, build_activate_request, collect_fingerprint};

use crate::error::ServerClientError;
use crate::flowy::FlowyApiClient;
use crate::paths::device_state_path;
use crate::session::ServerSession;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DeviceStateFile {
    #[serde(default)]
    sn: String,
    #[serde(default)]
    activation_uploaded: bool,
    #[serde(default)]
    activation_uploaded_app_version: String,
}

pub struct DeviceActivation {
    state_path: std::path::PathBuf,
}

impl DeviceActivation {
    pub fn new(hermes_home: impl AsRef<Path>) -> Self {
        Self {
            state_path: device_state_path(hermes_home.as_ref()),
        }
    }

    pub async fn try_activate_after_login(
        &self,
        api: &FlowyApiClient,
        session: &ServerSession,
    ) -> Result<bool, ServerClientError> {
        let app_version = env!("CARGO_PKG_VERSION");
        let mut state = self.load_state().await?;
        if state.activation_uploaded && state.activation_uploaded_app_version == app_version {
            debug!("device activation already reported for version {app_version}");
            return Ok(false);
        }

        let persisted_sn = if state.sn.is_empty() {
            None
        } else {
            Some(state.sn.as_str())
        };
        let fingerprint = collect_fingerprint(persisted_sn)?;
        if state.sn.is_empty() {
            state.sn = fingerprint.sn.clone();
        }

        let mut request = build_activate_request(api.config().channel.as_str(), &fingerprint);
        request.sn = state.sn.clone();

        match api.device_activate(session, &request).await {
            Ok(()) => {
                state.activation_uploaded = true;
                state.activation_uploaded_app_version = app_version.to_string();
                self.save_state(&state).await?;
                Ok(true)
            }
            Err(err) => {
                warn!(error = %err, "device activation upload failed");
                self.save_state(&state).await?;
                Err(err)
            }
        }
    }

    async fn load_state(&self) -> Result<DeviceStateFile, ServerClientError> {
        if let Some(parent) = self.state_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string()))
            })?;
        }
        if !tokio::fs::try_exists(&self.state_path)
            .await
            .unwrap_or(false)
        {
            return Ok(DeviceStateFile::default());
        }
        let raw = tokio::fs::read_to_string(&self.state_path)
            .await
            .map_err(|e| ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string())))?;
        serde_json::from_str(&raw).map_err(|e| ServerClientError::InvalidResponse(e.to_string()))
    }

    async fn save_state(&self, state: &DeviceStateFile) -> Result<(), ServerClientError> {
        if let Some(parent) = self.state_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string()))
            })?;
        }
        let raw = serde_json::to_string_pretty(state)
            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;
        tokio::fs::write(&self.state_path, raw)
            .await
            .map_err(|e| ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string())))
    }
}
