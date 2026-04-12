//! Skill management: SkillManager implementing the SkillProvider trait.

use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::errors::AgentError;
use hermes_core::traits::SkillProvider;
use hermes_core::types::{Skill, SkillMeta};
use tracing::{debug, info, instrument};

use crate::guard::SkillGuard;
use crate::hub::SkillsHubClient;
use crate::store::SkillStore;
use crate::version::compute_version;

// ---------------------------------------------------------------------------
// SkillError
// ---------------------------------------------------------------------------

/// Errors that can occur during skill operations.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("Skill not found: {0}")]
    NotFound(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Hub error: {0}")]
    HubError(String),

    #[error("Guard violation: {0}")]
    GuardViolation(String),
}

impl From<SkillError> for AgentError {
    fn from(err: SkillError) -> Self {
        AgentError::Config(err.to_string())
    }
}

impl From<std::io::Error> for SkillError {
    fn from(err: std::io::Error) -> Self {
        SkillError::Io(err.to_string())
    }
}

// ---------------------------------------------------------------------------
// SkillManager
// ---------------------------------------------------------------------------

/// Central manager for skill CRUD operations.
///
/// Delegates storage to a [`SkillStore`] implementation and optionally
/// synchronises with a remote [`SkillsHubClient`].
pub struct SkillManager {
    store: Arc<dyn SkillStore>,
    hub_client: Option<SkillsHubClient>,
    guard: SkillGuard,
}

impl SkillManager {
    /// Create a new `SkillManager` with the given local store.
    pub fn new(store: Arc<dyn SkillStore>) -> Self {
        Self {
            store,
            hub_client: None,
            guard: SkillGuard::default(),
        }
    }

    /// Create a `SkillManager` that also connects to a Skills Hub.
    pub fn with_hub(store: Arc<dyn SkillStore>, hub_client: SkillsHubClient) -> Self {
        Self {
            store,
            hub_client: Some(hub_client),
            guard: SkillGuard::default(),
        }
    }

    /// Replace the default guard with a custom one.
    pub fn with_guard(mut self, guard: SkillGuard) -> Self {
        self.guard = guard;
        self
    }
}

#[async_trait]
impl SkillProvider for SkillManager {
    #[instrument(skip(self, content), fields(name = %name))]
    async fn create_skill(
        &self,
        name: &str,
        content: &str,
        category: Option<&str>,
    ) -> Result<Skill, AgentError> {
        info!("Creating skill: {}", name);

        // Validate the skill content through the guard.
        let skill = Skill {
            name: name.to_string(),
            content: content.to_string(),
            category: category.map(String::from),
            description: None,
        };
        self.guard.validate_skill(&skill)?;

        // Save locally.
        self.store.save(&skill).await.map_err(|e| {
            tracing::error!("Failed to save skill {}: {}", name, e);
            AgentError::from(e)
        })?;

        // Optionally upload to hub.
        if let Some(ref hub) = self.hub_client {
            match hub.upload_skill(&skill).await {
                Ok(id) => debug!("Uploaded skill {} to hub with id {}", name, id),
                Err(e) => tracing::warn!("Failed to upload skill {} to hub: {}", name, e),
            }
        }

        Ok(skill)
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError> {
        debug!("Getting skill: {}", name);
        self.store
            .load(name)
            .await
            .map_err(AgentError::from)
    }

    #[instrument(skip(self))]
    async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
        debug!("Listing skills");
        self.store.list().await.map_err(AgentError::from)
    }

    #[instrument(skip(self, content), fields(name = %name))]
    async fn update_skill(
        &self,
        name: &str,
        content: &str,
    ) -> Result<Skill, AgentError> {
        info!("Updating skill: {}", name);

        // Load existing skill to preserve category / description.
        let mut skill = self
            .store
            .load(name)
            .await
            .map_err(AgentError::from)?
            .ok_or_else(|| SkillError::NotFound(name.to_string()))?;

        // Validate the new content.
        skill.content = content.to_string();
        self.guard.validate_skill(&skill)?;

        // Save updated version.
        self.store.save(&skill).await.map_err(|e| {
            tracing::error!("Failed to update skill {}: {}", name, e);
            AgentError::from(e)
        })?;

        // Optionally sync to hub.
        if let Some(ref hub) = self.hub_client {
            match hub.upload_skill(&skill).await {
                Ok(id) => debug!("Synced updated skill {} to hub: {}", name, id),
                Err(e) => tracing::warn!("Failed to sync updated skill {} to hub: {}", name, e),
            }
        }

        Ok(skill)
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn delete_skill(&self, name: &str) -> Result<(), AgentError> {
        info!("Deleting skill: {}", name);
        self.store.delete(name).await.map_err(AgentError::from)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::FileSkillStore;
    use std::path::PathBuf;
    use tempfile::tempdir;

    // Helper to create a manager backed by a temp dir.
    fn make_manager(dir: &PathBuf) -> SkillManager {
        let store = Arc::new(FileSkillStore::new(dir.clone()));
        SkillManager::new(store)
    }

    #[tokio::test]
    async fn test_create_and_get_skill() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(&dir.path().to_path_buf());

        let skill = mgr
            .create_skill("test-skill", "# Test Skill\nHello world", Some("general"))
            .await
            .unwrap();

        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.category.as_deref(), Some("general"));

        let loaded = mgr.get_skill("test-skill").await.unwrap().unwrap();
        assert_eq!(loaded.name, "test-skill");
        assert!(loaded.content.contains("Hello world"));
    }

    #[tokio::test]
    async fn test_list_skills() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(&dir.path().to_path_buf());

        mgr.create_skill("skill-a", "# Skill A\n1. Step one\n2. Step two", Some("cat1"))
            .await
            .unwrap();
        mgr.create_skill("skill-b", "# Skill B\n- Step one\n- Step two", None)
            .await
            .unwrap();

        let list = mgr.list_skills().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn test_update_skill() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(&dir.path().to_path_buf());

        mgr.create_skill("up-skill", "# Original\n1. Do something\n2. Do another thing", None)
            .await
            .unwrap();

        let updated = mgr.update_skill("up-skill", "# Updated\n1. New step").await.unwrap();
        assert!(updated.content.contains("# Updated"));

        let loaded = mgr.get_skill("up-skill").await.unwrap().unwrap();
        assert!(loaded.content.contains("# Updated"));
    }

    #[tokio::test]
    async fn test_delete_skill() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(&dir.path().to_path_buf());

        mgr.create_skill("del-skill", "# Bye\n- Step one\n- Step two", None).await.unwrap();
        mgr.delete_skill("del-skill").await.unwrap();

        let result = mgr.get_skill("del-skill").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_nonexistent_skill() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(&dir.path().to_path_buf());

        let result = mgr.get_skill("no-such-skill").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_skill() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(&dir.path().to_path_buf());

        let result = mgr.delete_skill("no-such-skill").await;
        // Deleting a non-existent skill should still succeed (idempotent).
        assert!(result.is_ok());
    }
}