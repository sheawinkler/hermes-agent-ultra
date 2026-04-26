//! Toolset system (Requirement 5)
//!
//! Manages named groups of tools (toolsets) with:
//! - Predefined toolset definitions for all built-in tool groups
//! - Recursive resolution with cycle detection
//! - Custom toolset creation at runtime
//! - Integration with ToolRegistry for plugin-registered toolsets

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::registry::ToolRegistry;

// ---------------------------------------------------------------------------
// Predefined toolset constants
// ---------------------------------------------------------------------------

/// Web search and extraction tools.
pub const TOOLSET_WEB: &[&str] = &["web_search", "web_extract"];
/// Terminal command execution tools.
pub const TOOLSET_TERMINAL: &[&str] = &["terminal", "process", "process_registry"];
/// File system tools.
pub const TOOLSET_FILE: &[&str] = &["read_file", "write_file", "patch", "search_files"];
/// Browser automation tools.
pub const TOOLSET_BROWSER: &[&str] = &[
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_scroll",
    "browser_back",
    "browser_press",
    "browser_get_images",
    "browser_vision",
    "browser_console",
];
/// Vision analysis tools.
pub const TOOLSET_VISION: &[&str] = &["vision_analyze"];
/// Image generation tools.
pub const TOOLSET_IMAGE_GEN: &[&str] = &["image_generate"];
/// Skills management tools.
pub const TOOLSET_SKILLS: &[&str] = &["skills_list", "skill_view", "skill_manage"];
/// Persistent memory tools.
pub const TOOLSET_MEMORY: &[&str] = &["memory"];
/// Session search tools.
pub const TOOLSET_SESSION_SEARCH: &[&str] = &["session_search"];
/// Todo/task management tools.
pub const TOOLSET_TODO: &[&str] = &["todo"];
/// Clarification/question tools.
pub const TOOLSET_CLARIFY: &[&str] = &["clarify"];
/// Code execution tools.
pub const TOOLSET_CODE_EXECUTION: &[&str] = &["execute_code"];
/// Task delegation tools.
pub const TOOLSET_DELEGATION: &[&str] = &["delegate_task"];
/// Cron job management tools.
pub const TOOLSET_CRONJOB: &[&str] = &["cronjob"];
/// Cross-platform messaging tools.
pub const TOOLSET_MESSAGING: &[&str] = &["send_message"];
/// Home Assistant integration tools.
pub const TOOLSET_HOMEASSISTANT: &[&str] = &[
    "ha_list_entities",
    "ha_get_state",
    "ha_list_services",
    "ha_call_service",
];
/// Text-to-speech tools.
pub const TOOLSET_TTS: &[&str] = &["text_to_speech", "tts_premium"];
/// Voice input/mode tools.
pub const TOOLSET_VOICE: &[&str] = &["transcription", "voice_mode"];
/// Security helpers.
pub const TOOLSET_SECURITY: &[&str] = &["osv_check", "url_safety"];
/// System utility helpers.
pub const TOOLSET_SYSTEM: &[&str] =
    &["env_passthrough", "credential_files", "tool_result_storage"];
/// Mixture-of-agents workflow.
pub const TOOLSET_MIXTURE_OF_AGENTS: &[&str] = &["mixture_of_agents"];

// ---------------------------------------------------------------------------
// Toolset
// ---------------------------------------------------------------------------

/// A named group of tools, optionally including other toolsets.
#[derive(Debug, Clone)]
pub struct Toolset {
    /// Toolset name (e.g. "web", "terminal").
    pub name: String,
    /// Tool names in this toolset.
    pub tools: Vec<String>,
    /// Names of other toolsets to include (resolved recursively).
    pub includes: Vec<String>,
}

impl Toolset {
    /// Create a new toolset with the given name and tools.
    pub fn new(name: impl Into<String>, tools: Vec<String>) -> Self {
        Self {
            name: name.into(),
            tools,
            includes: Vec::new(),
        }
    }

    /// Create a toolset that includes other toolsets.
    pub fn with_includes(name: impl Into<String>, includes: Vec<String>) -> Self {
        Self {
            name: name.into(),
            tools: Vec::new(),
            includes,
        }
    }

    /// Create a toolset with both tools and includes.
    pub fn new_mixed(name: impl Into<String>, tools: Vec<String>, includes: Vec<String>) -> Self {
        Self {
            name: name.into(),
            tools,
            includes,
        }
    }
}

// ---------------------------------------------------------------------------
// ToolsetManager
// ---------------------------------------------------------------------------

/// Manages toolset definitions and resolves them to flat lists of tool names.
pub struct ToolsetManager {
    /// Registered toolsets.
    toolsets: HashMap<String, Toolset>,
    /// Reference to the tool registry (for plugin toolset integration).
    registry: Arc<ToolRegistry>,
}

impl ToolsetManager {
    /// Create a new ToolsetManager with all predefined toolsets.
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        let mut manager = Self {
            toolsets: HashMap::new(),
            registry,
        };
        manager.register_defaults();
        manager
    }

    /// Register all predefined toolsets.
    fn register_defaults(&mut self) {
        self.register(Toolset::new(
            "web",
            TOOLSET_WEB.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "terminal",
            TOOLSET_TERMINAL.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "file",
            TOOLSET_FILE.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "browser",
            TOOLSET_BROWSER.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "vision",
            TOOLSET_VISION.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "image_gen",
            TOOLSET_IMAGE_GEN.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "skills",
            TOOLSET_SKILLS.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "memory",
            TOOLSET_MEMORY.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "session_search",
            TOOLSET_SESSION_SEARCH
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ));
        self.register(Toolset::new(
            "todo",
            TOOLSET_TODO.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "clarify",
            TOOLSET_CLARIFY.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "code_execution",
            TOOLSET_CODE_EXECUTION
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ));
        self.register(Toolset::new(
            "delegation",
            TOOLSET_DELEGATION.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "cronjob",
            TOOLSET_CRONJOB.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "messaging",
            TOOLSET_MESSAGING.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "homeassistant",
            TOOLSET_HOMEASSISTANT
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ));
        self.register(Toolset::new(
            "tts",
            TOOLSET_TTS.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "voice",
            TOOLSET_VOICE.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "security",
            TOOLSET_SECURITY.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "system",
            TOOLSET_SYSTEM.iter().map(|s| s.to_string()).collect(),
        ));
        self.register(Toolset::new(
            "mixture_of_agents",
            TOOLSET_MIXTURE_OF_AGENTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ));

        // Platform composite toolsets
        self.register(Toolset::with_includes(
            "hermes-cli",
            vec![
                "web",
                "terminal",
                "file",
                "browser",
                "vision",
                "image_gen",
                "skills",
                "memory",
                "session_search",
                "todo",
                "clarify",
                "code_execution",
                "delegation",
                "cronjob",
                "messaging",
                "homeassistant",
                "tts",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        ));
        self.register(Toolset::with_includes(
            "hermes-telegram",
            vec!["hermes-cli"].into_iter().map(String::from).collect(),
        ));
        self.register(Toolset::with_includes(
            "hermes-discord",
            vec!["hermes-telegram"]
                .into_iter()
                .map(String::from)
                .collect(),
        ));
        self.register(Toolset::with_includes(
            "hermes-whatsapp",
            vec!["hermes-telegram"]
                .into_iter()
                .map(String::from)
                .collect(),
        ));
        self.register(Toolset::with_includes(
            "hermes-slack",
            vec!["hermes-telegram"]
                .into_iter()
                .map(String::from)
                .collect(),
        ));
    }

    /// Register a toolset.
    pub fn register(&mut self, toolset: Toolset) {
        self.toolsets.insert(toolset.name.clone(), toolset);
    }

    /// Remove a toolset by name.
    pub fn deregister(&mut self, name: &str) {
        self.toolsets.remove(name);
    }

    /// Resolve a toolset name to a flat, deduplicated list of tool names.
    ///
    /// Handles:
    /// - Recursive resolution of `includes`
    /// - Cycle detection
    /// - "all" or "*" resolves to the union of all registered toolsets
    /// - Filters to only tools available in the registry (check_fn passes)
    pub fn resolve_toolset(&self, name: &str) -> Result<Vec<String>, ToolsetError> {
        let mut visited = HashSet::new();
        self.resolve_inner(name, &mut visited)
    }

    fn resolve_inner(
        &self,
        name: &str,
        visited: &mut HashSet<String>,
    ) -> Result<Vec<String>, ToolsetError> {
        // Handle "all" or "*" wildcard
        if name == "all" || name == "*" {
            let mut all_tools = HashSet::new();
            for ts_name in self.toolsets.keys() {
                // Each sub-toolset gets its own visited set to avoid
                // false cycle detection across independent branches.
                let mut sub_visited = HashSet::new();
                let tools = self.resolve_inner(ts_name, &mut sub_visited)?;
                all_tools.extend(tools);
            }
            let mut result: Vec<String> = all_tools.into_iter().collect();
            result.sort();
            return Ok(result);
        }

        // Cycle detection
        if visited.contains(name) {
            return Err(ToolsetError::CycleDetected(name.to_string()));
        }
        visited.insert(name.to_string());

        let toolset = self
            .toolsets
            .get(name)
            .ok_or_else(|| ToolsetError::NotFound(name.to_string()))?;

        let mut resolved = HashSet::new();

        // Add directly listed tools
        for tool in &toolset.tools {
            resolved.insert(tool.clone());
        }

        // Recursively resolve includes
        for include in &toolset.includes {
            let included_tools = self.resolve_inner(include, visited)?;
            for tool in included_tools {
                resolved.insert(tool);
            }
        }

        // Filter to only available tools in registry
        let available: Vec<String> = resolved
            .into_iter()
            .filter(|tool| self.registry.is_available(tool))
            .collect();

        let mut sorted = available;
        sorted.sort();
        Ok(sorted)
    }

    /// Resolve a toolset without availability filtering (includes all tools regardless of check_fn).
    pub fn resolve_toolset_unfiltered(&self, name: &str) -> Result<Vec<String>, ToolsetError> {
        let mut visited = HashSet::new();
        self.resolve_inner_unfiltered(name, &mut visited)
    }

    fn resolve_inner_unfiltered(
        &self,
        name: &str,
        visited: &mut HashSet<String>,
    ) -> Result<Vec<String>, ToolsetError> {
        if name == "all" || name == "*" {
            let mut all_tools = HashSet::new();
            for ts_name in self.toolsets.keys() {
                // Each sub-toolset gets its own visited set to avoid
                // false cycle detection across independent branches.
                let mut sub_visited = HashSet::new();
                let tools = self.resolve_inner_unfiltered(ts_name, &mut sub_visited)?;
                all_tools.extend(tools);
            }
            let mut result: Vec<String> = all_tools.into_iter().collect();
            result.sort();
            return Ok(result);
        }

        if visited.contains(name) {
            return Err(ToolsetError::CycleDetected(name.to_string()));
        }
        visited.insert(name.to_string());

        let toolset = self
            .toolsets
            .get(name)
            .ok_or_else(|| ToolsetError::NotFound(name.to_string()))?;

        let mut resolved = HashSet::new();
        for tool in &toolset.tools {
            resolved.insert(tool.clone());
        }
        for include in &toolset.includes {
            let included_tools = self.resolve_inner_unfiltered(include, visited)?;
            for tool in included_tools {
                resolved.insert(tool);
            }
        }

        let mut sorted: Vec<String> = resolved.into_iter().collect();
        sorted.sort();
        Ok(sorted)
    }

    /// Create a custom toolset at runtime.
    pub fn create_custom_toolset(&mut self, name: impl Into<String>, tools: Vec<String>) {
        self.register(Toolset::new(name, tools));
    }

    /// Get the list of all registered toolset names.
    pub fn list_toolsets(&self) -> Vec<String> {
        let mut names: Vec<String> = self.toolsets.keys().cloned().collect();
        names.sort();
        names
    }

    /// Get a reference to a toolset by name.
    pub fn get_toolset(&self, name: &str) -> Option<&Toolset> {
        self.toolsets.get(name)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during toolset resolution.
#[derive(Debug, thiserror::Error)]
pub enum ToolsetError {
    #[error("Toolset not found: {0}")]
    NotFound(String),
    #[error("Cycle detected in toolset resolution: {0}")]
    CycleDetected(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn empty_registry() -> Arc<ToolRegistry> {
        Arc::new(ToolRegistry::new())
    }

    #[test]
    fn test_default_toolsets_registered() {
        let manager = ToolsetManager::new(empty_registry());
        let names = manager.list_toolsets();
        assert!(names.contains(&"web".to_string()));
        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"file".to_string()));
    }

    #[test]
    fn test_resolve_web_toolset() {
        let manager = ToolsetManager::new(empty_registry());
        // Unfiltered since no tools are registered
        let tools = manager.resolve_toolset_unfiltered("web").unwrap();
        assert!(tools.contains(&"web_search".to_string()));
        assert!(tools.contains(&"web_extract".to_string()));
    }

    #[test]
    fn test_resolve_all() {
        let manager = ToolsetManager::new(empty_registry());
        let tools = manager.resolve_toolset_unfiltered("all").unwrap();
        // Should include tools from all toolsets
        assert!(tools.contains(&"web_search".to_string()));
        assert!(tools.contains(&"terminal".to_string()));
        assert!(tools.contains(&"read_file".to_string()));
    }

    #[test]
    fn test_cycle_detection() {
        let registry = empty_registry();
        let mut manager = ToolsetManager::new(registry.clone());
        // Create a cycle: a -> b -> a
        manager.register(Toolset::with_includes(
            "a_cycle",
            vec!["b_cycle".to_string()],
        ));
        manager.register(Toolset::with_includes(
            "b_cycle",
            vec!["a_cycle".to_string()],
        ));
        let result = manager.resolve_toolset("a_cycle");
        assert!(result.is_err());
    }

    #[test]
    fn test_not_found() {
        let manager = ToolsetManager::new(empty_registry());
        let result = manager.resolve_toolset("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_custom_toolset() {
        let mut manager = ToolsetManager::new(empty_registry());
        manager.create_custom_toolset(
            "my_custom",
            vec!["tool_a".to_string(), "tool_b".to_string()],
        );
        let tools = manager.resolve_toolset_unfiltered("my_custom").unwrap();
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn test_hermes_cli_toolset() {
        let manager = ToolsetManager::new(empty_registry());
        let tools = manager.resolve_toolset_unfiltered("hermes-cli").unwrap();
        // Should include tools from web, terminal, file, etc.
        assert!(tools.contains(&"web_search".to_string()));
        assert!(tools.contains(&"terminal".to_string()));
        assert!(tools.contains(&"read_file".to_string()));
        // Python parity core for CLI.
        assert!(tools.contains(&"image_generate".to_string()));
        assert!(tools.contains(&"session_search".to_string()));
        assert!(tools.contains(&"text_to_speech".to_string()));
        assert!(tools.contains(&"send_message".to_string()));
        assert!(tools.contains(&"ha_call_service".to_string()));
        assert!(tools.contains(&"cronjob".to_string()));
    }

    #[test]
    fn test_messaging_platform_presets_present() {
        let manager = ToolsetManager::new(empty_registry());
        for preset in [
            "hermes-telegram",
            "hermes-discord",
            "hermes-whatsapp",
            "hermes-slack",
        ] {
            let tools = manager.resolve_toolset_unfiltered(preset).unwrap();
            assert!(
                tools.contains(&"send_message".to_string()),
                "preset {preset} should include send_message"
            );
            assert!(
                tools.contains(&"terminal".to_string()),
                "preset {preset} should include terminal"
            );
            assert!(
                tools.contains(&"image_generate".to_string()),
                "preset {preset} should include image_generate"
            );
            assert!(
                tools.contains(&"cronjob".to_string()),
                "preset {preset} should include cronjob"
            );
            assert!(
                tools.contains(&"session_search".to_string()),
                "preset {preset} should include session_search"
            );
            assert!(
                tools.contains(&"text_to_speech".to_string()),
                "preset {preset} should include text_to_speech"
            );
            assert!(
                tools.contains(&"ha_call_service".to_string()),
                "preset {preset} should include homeassistant tools"
            );
        }
    }
}
