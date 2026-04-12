//! Plugin system for extending Hermes with custom functionality.
//!
//! Plugins can provide:
//! - Custom memory providers
//! - Additional tools
//! - Custom hooks
//! - Additional LLM providers

use std::collections::HashMap;
use std::sync::Arc;

use hermes_core::{AgentError, ToolHandler, ToolSchema};

/// Plugin metadata.
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
}

/// Trait for Hermes plugins.
#[async_trait::async_trait]
pub trait Plugin: Send + Sync {
    fn meta(&self) -> PluginMeta;
    async fn initialize(&self) -> Result<(), AgentError>;
    async fn shutdown(&self) -> Result<(), AgentError>;
    fn tools(&self) -> Vec<(ToolSchema, Arc<dyn ToolHandler>)> { Vec::new() }
}

/// Plugin manager.
pub struct PluginManager {
    plugins: HashMap<String, Arc<dyn Plugin>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self { plugins: HashMap::new() }
    }

    pub fn register(&mut self, plugin: Arc<dyn Plugin>) {
        let meta = plugin.meta();
        tracing::info!("Registered plugin: {} v{}", meta.name, meta.version);
        self.plugins.insert(meta.name.clone(), plugin);
    }

    pub async fn initialize_all(&self) -> Result<(), AgentError> {
        for (name, plugin) in &self.plugins {
            tracing::info!("Initializing plugin: {}", name);
            plugin.initialize().await?;
        }
        Ok(())
    }

    pub async fn shutdown_all(&self) -> Result<(), AgentError> {
        for (name, plugin) in &self.plugins {
            tracing::info!("Shutting down plugin: {}", name);
            if let Err(e) = plugin.shutdown().await {
                tracing::warn!("Plugin {} shutdown error: {}", name, e);
            }
        }
        Ok(())
    }

    pub fn all_tools(&self) -> Vec<(ToolSchema, Arc<dyn ToolHandler>)> {
        self.plugins.values().flat_map(|p| p.tools()).collect()
    }

    pub fn list_plugins(&self) -> Vec<PluginMeta> {
        self.plugins.values().map(|p| p.meta()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Plugin>> {
        self.plugins.get(name)
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestPlugin;

    #[async_trait::async_trait]
    impl Plugin for TestPlugin {
        fn meta(&self) -> PluginMeta {
            PluginMeta {
                name: "test".to_string(),
                version: "0.1.0".to_string(),
                description: "Test plugin".to_string(),
                author: None,
            }
        }
        async fn initialize(&self) -> Result<(), AgentError> { Ok(()) }
        async fn shutdown(&self) -> Result<(), AgentError> { Ok(()) }
    }

    #[test]
    fn test_plugin_register() {
        let mut mgr = PluginManager::new();
        mgr.register(Arc::new(TestPlugin));
        assert_eq!(mgr.list_plugins().len(), 1);
        assert_eq!(mgr.list_plugins()[0].name, "test");
    }
}
