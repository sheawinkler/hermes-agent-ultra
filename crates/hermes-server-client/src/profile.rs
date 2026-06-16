//! Cached user profile from `GET /user/me`.

use std::path::{Path, PathBuf};

use crate::error::ServerClientError;
use crate::flowy::UserMe;
use crate::paths::profile_cache_path;

pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(hermes_home: impl AsRef<Path>) -> Self {
        Self {
            path: profile_cache_path(hermes_home.as_ref()),
        }
    }

    pub async fn load(&self) -> Result<Option<UserMe>, ServerClientError> {
        if !tokio::fs::try_exists(&self.path).await.unwrap_or(false) {
            return Ok(None);
        }
        let raw = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|e| ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string())))?;
        let profile = serde_json::from_str(&raw)
            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;
        Ok(Some(profile))
    }

    pub async fn save(&self, profile: &UserMe) -> Result<(), ServerClientError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string()))
            })?;
        }
        let raw = serde_json::to_string_pretty(profile)
            .map_err(|e| ServerClientError::InvalidResponse(e.to_string()))?;
        tokio::fs::write(&self.path, raw)
            .await
            .map_err(|e| ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string())))
    }

    pub async fn clear(&self) -> Result<(), ServerClientError> {
        if tokio::fs::try_exists(&self.path).await.unwrap_or(false) {
            tokio::fs::remove_file(&self.path).await.map_err(|e| {
                ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string()))
            })?;
        }
        Ok(())
    }
}
