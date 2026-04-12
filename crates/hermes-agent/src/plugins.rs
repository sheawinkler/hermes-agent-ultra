//! Plugin system for extending Hermes with custom functionality.
//!
//! Plugins can provide:
//! - Custom memory providers
//! - Additional tools
//! - Custom hooks (pre/post LLM call, tool call, API request, session lifecycle)
//! - Additional LLM providers
//! - CLI commands

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use hermes_core::{AgentError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// HookType
// ---------------------------------------------------------------------------

/// Valid lifecycle hooks that plugins can register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookType {
    PreToolCall,
    PostToolCall,
    PreLlmCall,
    PostLlmCall,
    PreApiRequest,
    PostApiRequest,
    OnSessionStart,
    OnSessionEnd,
    OnSessionFinalize,
    OnSessionReset,
}

impl HookType {
    pub fn all() -> &'static [HookType] {
        &[
            HookType::PreToolCall,
            HookType::PostToolCall,
            HookType::PreLlmCall,
            HookType::PostLlmCall,
            HookType::PreApiRequest,
            HookType::PostApiRequest,
            HookType::OnSessionStart,
            HookType::OnSessionEnd,
            HookType::OnSessionFinalize,
            HookType::OnSessionReset,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            HookType::PreToolCall => "pre_tool_call",
            HookType::PostToolCall => "post_tool_call",
            HookType::PreLlmCall => "pre_llm_call",
            HookType::PostLlmCall => "post_llm_call",
            HookType::PreApiRequest => "pre_api_request",
            HookType::PostApiRequest => "post_api_request",
            HookType::OnSessionStart => "on_session_start",
            HookType::OnSessionEnd => "on_session_end",
            HookType::OnSessionFinalize => "on_session_finalize",
            HookType::OnSessionReset => "on_session_reset",
        }
    }
}

// ---------------------------------------------------------------------------
// HookResult
// ---------------------------------------------------------------------------

/// Hook callback result — allows hooks to inject context or signal errors.
#[derive(Debug, Clone)]
pub enum HookResult {
    /// Hook executed successfully with no side effects.
    Ok,
    /// Hook wants to inject additional context into the message stream.
    InjectContext(String),
    /// Hook encountered an error.
    Error(String),
}

// ---------------------------------------------------------------------------
// PluginManifest
// ---------------------------------------------------------------------------

/// Plugin manifest loaded from `plugin.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

// ---------------------------------------------------------------------------
// PluginCliCommand
// ---------------------------------------------------------------------------

/// A CLI command contributed by a plugin.
#[derive(Debug, Clone)]
pub struct PluginCliCommand {
    pub name: String,
    pub description: String,
    pub plugin_name: String,
}

// ---------------------------------------------------------------------------
// ContextEngine trait (for plugins that want to inject context)
// ---------------------------------------------------------------------------

/// Trait for context engines that plugins can provide.
pub trait ContextEngine: Send + Sync {
    fn inject(&self, query: &str) -> Option<String>;
}

// ---------------------------------------------------------------------------
// PluginContext
// ---------------------------------------------------------------------------

/// Plugin context provided to plugins during registration.
/// Plugins use this to register hooks, tools, and CLI commands.
pub struct PluginContext {
    hooks: HashMap<HookType, Vec<Arc<dyn Fn(&Value) -> HookResult + Send + Sync>>>,
    tools: Vec<(ToolSchema, Arc<dyn ToolHandler>)>,
    cli_commands: Vec<PluginCliCommand>,
    context_engine: Option<Arc<dyn ContextEngine>>,
    injected_messages: Vec<String>,
}

impl PluginContext {
    pub fn new() -> Self {
        Self {
            hooks: HashMap::new(),
            tools: Vec::new(),
            cli_commands: Vec::new(),
            context_engine: None,
            injected_messages: Vec::new(),
        }
    }

    /// Register a hook callback for a specific lifecycle event.
    pub fn on(
        &mut self,
        hook: HookType,
        callback: Arc<dyn Fn(&Value) -> HookResult + Send + Sync>,
    ) {
        self.hooks.entry(hook).or_default().push(callback);
    }

    /// Register a tool provided by the plugin.
    pub fn register_tool(&mut self, schema: ToolSchema, handler: Arc<dyn ToolHandler>) {
        self.tools.push((schema, handler));
    }

    /// Register a CLI command provided by the plugin.
    pub fn register_cli_command(&mut self, cmd: PluginCliCommand) {
        self.cli_commands.push(cmd);
    }

    /// Set a context engine for this plugin.
    pub fn set_context_engine(&mut self, engine: Arc<dyn ContextEngine>) {
        self.context_engine = Some(engine);
    }

    /// Inject a system message into the conversation.
    pub fn inject_message(&mut self, message: String) {
        self.injected_messages.push(message);
    }

    pub fn drain_injected_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.injected_messages)
    }
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// PluginMeta
// ---------------------------------------------------------------------------

/// Plugin metadata.
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
}

impl From<PluginManifest> for PluginMeta {
    fn from(m: PluginManifest) -> Self {
        Self {
            name: m.name,
            version: m.version,
            description: m.description,
            author: m.author,
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin trait
// ---------------------------------------------------------------------------

/// Trait for Hermes plugins.
#[async_trait::async_trait]
pub trait Plugin: Send + Sync {
    fn meta(&self) -> PluginMeta;
    async fn initialize(&self) -> Result<(), AgentError>;
    async fn shutdown(&self) -> Result<(), AgentError>;
    fn tools(&self) -> Vec<(ToolSchema, Arc<dyn ToolHandler>)> {
        Vec::new()
    }

    /// Called during registration to let the plugin register hooks, tools, etc.
    fn register(&self, _ctx: &mut PluginContext) {}
}

// ---------------------------------------------------------------------------
// PluginManager
// ---------------------------------------------------------------------------

/// Plugin manager — central registry for all loaded plugins.
pub struct PluginManager {
    plugins: HashMap<String, Arc<dyn Plugin>>,
    context: PluginContext,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            context: PluginContext::new(),
        }
    }

    pub fn register(&mut self, plugin: Arc<dyn Plugin>) {
        let meta = plugin.meta();
        tracing::info!("Registered plugin: {} v{}", meta.name, meta.version);
        plugin.register(&mut self.context);
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
        let mut tools: Vec<_> = self.plugins.values().flat_map(|p| p.tools()).collect();
        tools.extend(self.context.tools.iter().cloned());
        tools
    }

    pub fn list_plugins(&self) -> Vec<PluginMeta> {
        self.plugins.values().map(|p| p.meta()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Plugin>> {
        self.plugins.get(name)
    }

    /// Invoke all registered hooks for the given lifecycle event.
    pub fn invoke_hook(&self, hook: HookType, context: &Value) -> Vec<HookResult> {
        let Some(callbacks) = self.context.hooks.get(&hook) else {
            return Vec::new();
        };
        callbacks.iter().map(|cb| cb(context)).collect()
    }

    /// Get all tools registered via plugin contexts.
    pub fn get_plugin_tools(&self) -> Vec<(ToolSchema, Arc<dyn ToolHandler>)> {
        self.context.tools.clone()
    }

    /// Get all CLI commands registered via plugin contexts.
    pub fn get_plugin_cli_commands(&self) -> Vec<PluginCliCommand> {
        self.context.cli_commands.clone()
    }

    /// Check if a plugin is disabled.
    pub fn is_disabled(&self, name: &str, disabled_list: &[String]) -> bool {
        disabled_list.iter().any(|d| d == name)
    }

    /// Discover plugins in the given hermes directory by scanning for `plugin.yaml` files.
    pub fn discover_plugins(hermes_dir: &Path) -> Vec<(PluginManifest, std::path::PathBuf)> {
        let plugins_dir = hermes_dir.join("plugins");
        let mut discovered = Vec::new();

        if !plugins_dir.exists() {
            return discovered;
        }

        let Ok(entries) = std::fs::read_dir(&plugins_dir) else {
            return discovered;
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("plugin.yaml");
            if !manifest_path.exists() {
                continue;
            }

            let disabled_marker = path.join(".disabled");
            if disabled_marker.exists() {
                tracing::debug!("Skipping disabled plugin: {}", path.display());
                continue;
            }

            match std::fs::read_to_string(&manifest_path) {
                Ok(content) => match serde_yaml::from_str::<PluginManifest>(&content) {
                    Ok(manifest) => {
                        tracing::debug!(
                            "Discovered plugin: {} v{} at {}",
                            manifest.name,
                            manifest.version,
                            path.display()
                        );
                        discovered.push((manifest, path));
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to parse plugin.yaml at {}: {}",
                            manifest_path.display(),
                            e
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "Failed to read plugin.yaml at {}: {}",
                        manifest_path.display(),
                        e
                    );
                }
            }
        }

        discovered
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
        async fn initialize(&self) -> Result<(), AgentError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), AgentError> {
            Ok(())
        }
    }

    struct HookPlugin;

    #[async_trait::async_trait]
    impl Plugin for HookPlugin {
        fn meta(&self) -> PluginMeta {
            PluginMeta {
                name: "hook_test".to_string(),
                version: "0.1.0".to_string(),
                description: "Hook test plugin".to_string(),
                author: None,
            }
        }
        async fn initialize(&self) -> Result<(), AgentError> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<(), AgentError> {
            Ok(())
        }
        fn register(&self, ctx: &mut PluginContext) {
            ctx.on(
                HookType::PreLlmCall,
                Arc::new(|_ctx| HookResult::InjectContext("injected by hook".to_string())),
            );
        }
    }

    #[test]
    fn test_plugin_register() {
        let mut mgr = PluginManager::new();
        mgr.register(Arc::new(TestPlugin));
        assert_eq!(mgr.list_plugins().len(), 1);
        assert_eq!(mgr.list_plugins()[0].name, "test");
    }

    #[test]
    fn test_hook_invocation() {
        let mut mgr = PluginManager::new();
        mgr.register(Arc::new(HookPlugin));
        let results = mgr.invoke_hook(HookType::PreLlmCall, &serde_json::json!({}));
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], HookResult::InjectContext(_)));
    }

    #[test]
    fn test_invoke_hook_no_handlers() {
        let mgr = PluginManager::new();
        let results = mgr.invoke_hook(HookType::OnSessionStart, &serde_json::json!({}));
        assert!(results.is_empty());
    }

    #[test]
    fn test_is_disabled() {
        let mgr = PluginManager::new();
        let disabled = vec!["foo".to_string(), "bar".to_string()];
        assert!(mgr.is_disabled("foo", &disabled));
        assert!(!mgr.is_disabled("baz", &disabled));
    }

    #[test]
    fn test_plugin_context_inject_message() {
        let mut ctx = PluginContext::new();
        ctx.inject_message("hello".to_string());
        ctx.inject_message("world".to_string());
        let msgs = ctx.drain_injected_messages();
        assert_eq!(msgs.len(), 2);
        assert!(ctx.drain_injected_messages().is_empty());
    }

    #[test]
    fn test_hook_type_as_str() {
        assert_eq!(HookType::PreToolCall.as_str(), "pre_tool_call");
        assert_eq!(HookType::OnSessionEnd.as_str(), "on_session_end");
    }

    #[test]
    fn test_manifest_from_yaml() {
        let yaml = r#"
name: test-plugin
version: "1.0.0"
description: A test plugin
author: Test Author
dependencies:
  - dep-a
  - dep-b
"#;
        let manifest: PluginManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.name, "test-plugin");
        assert_eq!(manifest.dependencies.len(), 2);
    }

    #[test]
    fn test_plugin_meta_from_manifest() {
        let manifest = PluginManifest {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            description: "desc".to_string(),
            author: Some("me".to_string()),
            homepage: None,
            dependencies: vec![],
        };
        let meta: PluginMeta = manifest.into();
        assert_eq!(meta.name, "test");
        assert_eq!(meta.author.unwrap(), "me");
    }
}
