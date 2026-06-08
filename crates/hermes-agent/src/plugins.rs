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
    TransformLlmOutput,
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
            HookType::TransformLlmOutput,
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
            HookType::TransformLlmOutput => "transform_llm_output",
        }
    }
}

// ---------------------------------------------------------------------------
// Hook payload schema validation
// ---------------------------------------------------------------------------

fn expect_obj(ctx: &Value, hook: HookType) -> Result<&serde_json::Map<String, Value>, String> {
    ctx.as_object()
        .ok_or_else(|| format!("{} context must be a JSON object", hook.as_str()))
}

fn require_type(
    obj: &serde_json::Map<String, Value>,
    key: &str,
    type_name: &str,
    check: impl Fn(&Value) -> bool,
) -> Result<(), String> {
    let Some(v) = obj.get(key) else {
        return Err(format!("missing required field: {}", key));
    };
    if !check(v) {
        return Err(format!("field '{}' must be {}", key, type_name));
    }
    Ok(())
}

fn optional_string_or_null(obj: &serde_json::Map<String, Value>, key: &str) -> Result<(), String> {
    if let Some(v) = obj.get(key) {
        if !(v.is_null() || v.is_string()) {
            return Err(format!("field '{}' must be string|null", key));
        }
    }
    Ok(())
}

pub(crate) fn validate_hook_payload(hook: HookType, context: &Value) -> Result<(), String> {
    let obj = expect_obj(context, hook)?;
    match hook {
        HookType::PreToolCall => {
            require_type(obj, "tool", "string", Value::is_string)?;
            require_type(obj, "turn", "number", Value::is_number)?;
        }
        HookType::PostToolCall => {
            require_type(obj, "tool", "string", Value::is_string)?;
            require_type(obj, "is_error", "boolean", Value::is_boolean)?;
            require_type(obj, "turn", "number", Value::is_number)?;
        }
        HookType::PreLlmCall => {
            require_type(obj, "turn", "number", Value::is_number)?;
            require_type(obj, "model", "string", Value::is_string)?;
        }
        HookType::PostLlmCall => {
            require_type(obj, "turn", "number", Value::is_number)?;
            require_type(obj, "api_time_ms", "number", Value::is_number)?;
            require_type(obj, "has_tool_calls", "boolean", Value::is_boolean)?;
        }
        HookType::PreApiRequest => {
            let has_attempt = obj.get("attempt").map(Value::is_number).unwrap_or(false);
            let has_api_call_count = obj
                .get("api_call_count")
                .map(Value::is_number)
                .unwrap_or(false);
            if !has_attempt && !has_api_call_count {
                return Err(
                    "missing required field: attempt (or api_call_count)".to_string(),
                );
            }
            require_type(obj, "model", "string", Value::is_string)?;
            if let Some(v) = obj.get("stream") {
                if !v.is_boolean() {
                    return Err("field 'stream' must be boolean".to_string());
                }
            }
            optional_string_or_null(obj, "route_label")?;
        }
        HookType::PostApiRequest => {
            require_type(obj, "attempt", "number", Value::is_number)?;
            require_type(obj, "model", "string", Value::is_string)?;
            require_type(obj, "stream", "boolean", Value::is_boolean)?;
            require_type(obj, "ok", "boolean", Value::is_boolean)?;
            optional_string_or_null(obj, "finish_reason")?;
            optional_string_or_null(obj, "error")?;
            if let Some(v) = obj.get("has_tool_calls") {
                if !v.is_boolean() {
                    return Err("field 'has_tool_calls' must be boolean".to_string());
                }
            }
            if let Some(v) = obj.get("interrupted") {
                if !v.is_boolean() {
                    return Err("field 'interrupted' must be boolean".to_string());
                }
            }
        }
        HookType::OnSessionStart => {
            require_type(obj, "model", "string", Value::is_string)?;
            optional_string_or_null(obj, "session_id")?;
        }
        HookType::OnSessionEnd => {
            let has_finished = obj
                .get("finished_naturally")
                .map(Value::is_boolean)
                .unwrap_or(false);
            let has_completed = obj.get("completed").map(Value::is_boolean).unwrap_or(false);
            if !has_finished && !has_completed {
                return Err(
                    "missing required field: finished_naturally (or completed)".to_string(),
                );
            }
            require_type(obj, "interrupted", "boolean", Value::is_boolean)?;
            if let Some(v) = obj.get("turns") {
                if !v.is_number() {
                    return Err("field 'turns' must be number".to_string());
                }
            }
            if let Some(v) = obj.get("session_started_hooks_fired") {
                if !v.is_boolean() {
                    return Err("field 'session_started_hooks_fired' must be boolean".to_string());
                }
            }
            optional_string_or_null(obj, "session_id")?;
        }
        HookType::OnSessionFinalize => {
            require_type(obj, "turns", "number", Value::is_number)?;
            require_type(obj, "tool_errors", "number", Value::is_number)?;
            require_type(obj, "session_cost_usd", "number", Value::is_number)?;
            optional_string_or_null(obj, "session_id")?;
        }
        HookType::OnSessionReset => {
            require_type(obj, "turns", "number", Value::is_number)?;
            require_type(obj, "source", "string", Value::is_string)?;
            optional_string_or_null(obj, "session_id")?;
        }
        HookType::TransformLlmOutput => {
            require_type(obj, "content", "string", Value::is_string)?;
        }
    }
    Ok(())
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
    /// Hook wants to rewrite final assistant text before delivery.
    TransformLlmOutput(String),
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
    pub kind: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

/// Plugin source used during discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginDiscoverySource {
    User,
    Project,
}

impl PluginDiscoverySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginDiscoverySource::User => "user",
            PluginDiscoverySource::Project => "project",
        }
    }
}

/// A discovered plugin bundle with source metadata.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub path: std::path::PathBuf,
    pub source: PluginDiscoverySource,
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

    pub fn has_hooks(&self) -> bool {
        !self.hooks.is_empty()
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

    /// Register a hook callback directly (e.g. config-driven shell hooks).
    pub fn register_hook_callback(
        &mut self,
        hook: HookType,
        callback: Arc<dyn Fn(&Value) -> HookResult + Send + Sync>,
    ) {
        self.context.on(hook, callback);
    }

    /// True when any lifecycle hook callbacks are registered.
    pub fn has_hooks(&self) -> bool {
        self.context.has_hooks()
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
        if let Err(err) = validate_hook_payload(hook, context) {
            tracing::warn!(
                hook = %hook.as_str(),
                error = %err,
                "Hook payload does not match recommended schema"
            );
        }
        callbacks
            .iter()
            .filter_map(|cb| {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(context))) {
                    Ok(result) => Some(result),
                    Err(_) => {
                        tracing::warn!(hook = hook.as_str(), "Plugin hook panicked");
                        None
                    }
                }
            })
            .collect()
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

    /// Discover plugins in the given Hermes directory by scanning `plugins/`.
    ///
    /// Backwards-compatible wrapper that preserves the historical return type
    /// and only scans the user plugin directory.
    pub fn discover_plugins(hermes_dir: &Path) -> Vec<(PluginManifest, std::path::PathBuf)> {
        Self::discover_plugins_with_options(hermes_dir, None, false)
            .into_iter()
            .map(|entry| (entry.manifest, entry.path))
            .collect()
    }

    /// Discover plugins with explicit source controls.
    ///
    /// - Always scans user plugins at `<hermes_dir>/plugins`.
    /// - Optionally scans project plugins at `<cwd>/.hermes-agent-ultra/plugins` when
    ///   `enable_project_plugins` is true.
    pub fn discover_plugins_with_options(
        hermes_dir: &Path,
        cwd: Option<&Path>,
        enable_project_plugins: bool,
    ) -> Vec<DiscoveredPlugin> {
        let mut discovered = Vec::new();
        let user_plugins_dir = hermes_dir.join("plugins");
        scan_plugin_root(
            &user_plugins_dir,
            PluginDiscoverySource::User,
            &mut discovered,
        );

        if enable_project_plugins {
            if let Some(workdir) = cwd {
                let project_plugins_dir =
                    hermes_config::project_hermes_dir(workdir).join("plugins");
                scan_plugin_root(
                    &project_plugins_dir,
                    PluginDiscoverySource::Project,
                    &mut discovered,
                );
            }
        }

        discovered
    }

    /// Discover plugins using runtime defaults.
    ///
    /// Project-local plugins are included when
    /// `HERMES_ENABLE_PROJECT_PLUGINS` is enabled.
    pub fn discover_plugins_runtime_default(hermes_dir: &Path) -> Vec<DiscoveredPlugin> {
        let enable_project_plugins = std::env::var("HERMES_ENABLE_PROJECT_PLUGINS")
            .ok()
            .is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let cwd = std::env::current_dir().ok();
        Self::discover_plugins_with_options(hermes_dir, cwd.as_deref(), enable_project_plugins)
    }

    /// Build a plugin manager with built-in and config-driven shell hooks.
    ///
    /// Returns `None` when no hooks or native plugins are registered (caller
    /// may skip [`AgentLoop::with_plugins`]).
    pub fn build_runtime_manager(hermes_home: &Path) -> Option<std::sync::Arc<std::sync::Mutex<Self>>> {
        let mut mgr = Self::new();
        register_builtin_plugins(&mut mgr);
        crate::shell_hooks::register_config_shell_hooks(&mut mgr, hermes_home);
        if mgr.list_plugins().is_empty() && !mgr.has_hooks() {
            return None;
        }
        Some(std::sync::Arc::new(std::sync::Mutex::new(mgr)))
    }
}

/// Register native Rust plugins compiled into the agent binary.
pub fn register_builtin_plugins(_mgr: &mut PluginManager) {
    // Extension point for future in-tree plugins (observability, disk-cleanup, …).
}

fn scan_plugin_root(
    plugins_dir: &Path,
    source: PluginDiscoverySource,
    discovered: &mut Vec<DiscoveredPlugin>,
) {
    if !plugins_dir.exists() {
        return;
    }

    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return;
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
                Ok(mut manifest) => {
                    // If the manifest doesn't explicitly declare a plugin kind,
                    // detect Python memory-provider plugins and auto-coerce them
                    // to `exclusive` so they can be routed away from generic
                    // plugin loading.
                    let explicit_kind = manifest
                        .kind
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    if explicit_kind.is_none() {
                        let init_file = path.join("__init__.py");
                        if let Ok(source_text) = std::fs::read_to_string(&init_file) {
                            let probe = if source_text.len() > 8192 {
                                &source_text[..8192]
                            } else {
                                source_text.as_str()
                            };
                            if probe.contains("register_memory_provider")
                                || probe.contains("MemoryProvider")
                            {
                                manifest.kind = Some("exclusive".to_string());
                                tracing::debug!(
                                    "Plugin {} auto-coerced to kind=exclusive (memory provider heuristic)",
                                    manifest.name
                                );
                            }
                        }
                    }
                    tracing::debug!(
                        "Discovered plugin: {} v{} at {} (source={})",
                        manifest.name,
                        manifest.version,
                        path.display(),
                        source.as_str()
                    );
                    discovered.push(DiscoveredPlugin {
                        manifest,
                        path,
                        source,
                    });
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

    struct SessionEndHookPlugin;

    #[async_trait::async_trait]
    impl Plugin for SessionEndHookPlugin {
        fn meta(&self) -> PluginMeta {
            PluginMeta {
                name: "session_end_hook_test".to_string(),
                version: "0.1.0".to_string(),
                description: "on_session_end hook test".to_string(),
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
                HookType::OnSessionEnd,
                Arc::new(|ctx_val| {
                    let completed = ctx_val
                        .get("completed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let interrupted = ctx_val
                        .get("interrupted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    HookResult::InjectContext(format!(
                        "session_end:{completed}:{interrupted}"
                    ))
                }),
            );
        }
    }

    struct PreApiRequestHookPlugin;

    #[async_trait::async_trait]
    impl Plugin for PreApiRequestHookPlugin {
        fn meta(&self) -> PluginMeta {
            PluginMeta {
                name: "pre_api_request_hook_test".to_string(),
                version: "0.1.0".to_string(),
                description: "pre_api_request hook test".to_string(),
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
                HookType::PreApiRequest,
                Arc::new(|ctx_val| {
                    let count = ctx_val
                        .get("api_call_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    HookResult::InjectContext(format!("pre_api_request:{count}"))
                }),
            );
        }
    }

    struct PanicHookPlugin;

    #[async_trait::async_trait]
    impl Plugin for PanicHookPlugin {
        fn meta(&self) -> PluginMeta {
            PluginMeta {
                name: "panic_hook_test".to_string(),
                version: "0.1.0".to_string(),
                description: "Panic hook test plugin".to_string(),
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
                HookType::OnSessionFinalize,
                Arc::new(|_ctx| panic!("hook failed")),
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
    fn test_on_session_end_hook_invocation() {
        let mut mgr = PluginManager::new();
        mgr.register(Arc::new(SessionEndHookPlugin));
        let results = mgr.invoke_hook(
            HookType::OnSessionEnd,
            &serde_json::json!({
                "session_id": "sess-1",
                "completed": true,
                "interrupted": false,
                "model": "gpt-4o",
                "platform": "cli",
            }),
        );
        assert_eq!(results.len(), 1);
        assert!(matches!(
            &results[0],
            HookResult::InjectContext(text) if text == "session_end:true:false"
        ));
    }

    #[test]
    fn test_pre_api_request_hook_invocation() {
        let mut mgr = PluginManager::new();
        mgr.register(Arc::new(PreApiRequestHookPlugin));
        let results = mgr.invoke_hook(
            HookType::PreApiRequest,
            &serde_json::json!({
                "session_id": "sess-1",
                "api_call_count": 2,
                "model": "claude-sonnet-4-6",
                "provider": "anthropic",
            }),
        );
        assert_eq!(results.len(), 1);
        assert!(matches!(
            &results[0],
            HookResult::InjectContext(text) if text == "pre_api_request:2"
        ));
    }

    #[test]
    fn test_invoke_hook_contains_panics() {
        let mut mgr = PluginManager::new();
        mgr.register(Arc::new(PanicHookPlugin));
        let results = mgr.invoke_hook(
            HookType::OnSessionFinalize,
            &serde_json::json!({"session_id": "test", "platform": "cli"}),
        );
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
        assert_eq!(HookType::OnSessionFinalize.as_str(), "on_session_finalize");
        assert_eq!(HookType::OnSessionReset.as_str(), "on_session_reset");
        assert!(HookType::all().contains(&HookType::OnSessionFinalize));
        assert!(HookType::all().contains(&HookType::OnSessionReset));
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
            kind: None,
            author: Some("me".to_string()),
            homepage: None,
            dependencies: vec![],
        };
        let meta: PluginMeta = manifest.into();
        assert_eq!(meta.name, "test");
        assert_eq!(meta.author.unwrap(), "me");
    }

    #[test]
    fn test_discover_plugins_auto_coerces_memory_provider_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins").join("mempalace");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.yaml"),
            "name: mempalace\nversion: \"0.1.0\"\ndescription: Test\n",
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("__init__.py"),
            "class MemPalaceProvider: pass\n\ndef register(ctx):\n    ctx.register_memory_provider('mempalace', MemPalaceProvider)\n",
        )
        .unwrap();

        let discovered = PluginManager::discover_plugins(tmp.path());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].0.kind.as_deref(), Some("exclusive"));
    }

    #[test]
    fn test_discover_plugins_explicit_standalone_not_overridden() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins").join("not_memory");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.yaml"),
            "name: not_memory\nversion: \"0.1.0\"\ndescription: Test\nkind: standalone\n",
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("__init__.py"),
            "# MemoryProvider mentioned for docs only\n",
        )
        .unwrap();

        let discovered = PluginManager::discover_plugins(tmp.path());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].0.kind.as_deref(), Some("standalone"));
    }

    #[test]
    fn test_discover_plugins_preserves_model_provider_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins").join("test-model-provider");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.yaml"),
            "name: test-model-provider\nversion: \"0.1.0\"\ndescription: Test\nkind: model-provider\n",
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("__init__.py"),
            "raise AssertionError('model-provider plugins are profile manifests, not generic runtime plugins')\n",
        )
        .unwrap();

        let discovered = PluginManager::discover_plugins(tmp.path());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].0.kind.as_deref(), Some("model-provider"));
    }

    #[test]
    fn test_discover_plugins_with_options_includes_project_when_enabled() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let user_plugin = home.path().join("plugins").join("user_bundle");
        std::fs::create_dir_all(&user_plugin).unwrap();
        std::fs::write(
            user_plugin.join("plugin.yaml"),
            "name: user_bundle\nversion: \"0.1.0\"\ndescription: User plugin\n",
        )
        .unwrap();

        let project_plugin = hermes_config::project_hermes_dir(cwd.path())
            .join("plugins")
            .join("project_bundle");
        std::fs::create_dir_all(&project_plugin).unwrap();
        std::fs::write(
            project_plugin.join("plugin.yaml"),
            "name: project_bundle\nversion: \"0.1.0\"\ndescription: Project plugin\n",
        )
        .unwrap();

        let discovered =
            PluginManager::discover_plugins_with_options(home.path(), Some(cwd.path()), true);
        assert_eq!(discovered.len(), 2);
        assert!(discovered
            .iter()
            .any(|d| d.manifest.name == "user_bundle" && d.source == PluginDiscoverySource::User));
        assert!(discovered
            .iter()
            .any(|d| d.manifest.name == "project_bundle"
                && d.source == PluginDiscoverySource::Project));
    }

    #[test]
    fn test_discover_plugins_with_options_excludes_project_when_disabled() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let user_plugin = home.path().join("plugins").join("user_bundle");
        std::fs::create_dir_all(&user_plugin).unwrap();
        std::fs::write(
            user_plugin.join("plugin.yaml"),
            "name: user_bundle\nversion: \"0.1.0\"\ndescription: User plugin\n",
        )
        .unwrap();

        let project_plugin = hermes_config::project_hermes_dir(cwd.path())
            .join("plugins")
            .join("project_bundle");
        std::fs::create_dir_all(&project_plugin).unwrap();
        std::fs::write(
            project_plugin.join("plugin.yaml"),
            "name: project_bundle\nversion: \"0.1.0\"\ndescription: Project plugin\n",
        )
        .unwrap();

        let discovered =
            PluginManager::discover_plugins_with_options(home.path(), Some(cwd.path()), false);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].manifest.name, "user_bundle");
        assert_eq!(discovered[0].source, PluginDiscoverySource::User);
    }

    #[test]
    fn test_validate_hook_payload_accepts_pre_api_request() {
        let ctx = serde_json::json!({
            "attempt": 0,
            "model": "gpt-4o",
            "stream": false,
            "route_label": null
        });
        assert!(validate_hook_payload(HookType::PreApiRequest, &ctx).is_ok());
    }

    #[test]
    fn test_validate_hook_payload_rejects_missing_required_field() {
        let ctx = serde_json::json!({
            "model": "gpt-4o",
            "stream": false
        });
        let err = validate_hook_payload(HookType::PreApiRequest, &ctx).unwrap_err();
        assert!(err.contains("attempt"));
    }

    #[test]
    fn test_validate_hook_payload_accepts_api_call_count() {
        let ctx = serde_json::json!({
            "api_call_count": 2,
            "model": "gpt-4o"
        });
        assert!(validate_hook_payload(HookType::PreApiRequest, &ctx).is_ok());
    }

    #[test]
    fn test_validate_hook_payload_accepts_on_session_end_completed() {
        let ctx = serde_json::json!({
            "completed": true,
            "interrupted": false
        });
        assert!(validate_hook_payload(HookType::OnSessionEnd, &ctx).is_ok());
    }

    #[test]
    fn test_hook_payload_golden_fixtures() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_payloads");
        for hook in HookType::all() {
            let path = root.join(format!("{}.json", hook.as_str()));
            assert!(path.is_file(), "missing fixture for {}", hook.as_str());
            let ctx: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            validate_hook_payload(*hook, &ctx)
                .unwrap_or_else(|e| panic!("{}: {e}", hook.as_str()));
        }
    }

    fn test_invoke_hook_keeps_backward_compat_even_with_invalid_payload() {
        let mut mgr = PluginManager::new();
        let hit = Arc::new(std::sync::Mutex::new(0u32));
        let hit_ref = hit.clone();
        mgr.context.hooks.insert(
            HookType::PreApiRequest,
            vec![Arc::new(move |_ctx| {
                *hit_ref.lock().expect("counter lock") += 1;
                HookResult::Ok
            })],
        );
        let _ = mgr.invoke_hook(HookType::PreApiRequest, &serde_json::json!({}));
        assert_eq!(*hit.lock().expect("counter lock"), 1);
    }
}
