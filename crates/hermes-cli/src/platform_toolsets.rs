//! Helpers for resolving per-platform toolset configuration.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hermes_config::GatewayConfig;
use hermes_core::ToolSchema;
use hermes_tools::{ToolRegistry, ToolsetManager};

/// Normalize platform aliases used by runtime adapters to config keys.
pub fn normalize_platform_key(platform: &str) -> String {
    match platform.trim().to_ascii_lowercase().as_str() {
        "local" => "cli".to_string(),
        "tg" => "telegram".to_string(),
        "dc" => "discord".to_string(),
        other => other.to_string(),
    }
}

/// Python-parity defaults when no explicit `platform_toolsets` entry exists.
pub fn default_platform_toolsets() -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    map.insert("cli".to_string(), vec!["hermes-cli".to_string()]);
    map.insert("telegram".to_string(), vec!["hermes-telegram".to_string()]);
    map.insert("discord".to_string(), vec!["hermes-discord".to_string()]);
    map.insert("whatsapp".to_string(), vec!["hermes-whatsapp".to_string()]);
    map.insert("slack".to_string(), vec!["hermes-slack".to_string()]);
    map
}

/// Configured toolset tokens for a platform, with default fallback.
pub fn configured_platform_toolsets(config: &GatewayConfig, platform: &str) -> Vec<String> {
    let key = normalize_platform_key(platform);
    if let Some(custom) = config.platform_toolsets.get(&key) {
        if !custom.is_empty() {
            return custom.clone();
        }
    }
    default_platform_toolsets()
        .remove(&key)
        .unwrap_or_else(|| vec!["hermes-telegram".to_string()])
}

/// Resolve tool names allowed for this platform based on configured toolsets.
pub fn resolve_platform_tool_names(
    config: &GatewayConfig,
    platform: &str,
    registry: &Arc<ToolRegistry>,
) -> Vec<String> {
    let requested = configured_platform_toolsets(config, platform);
    let manager = ToolsetManager::new(Arc::clone(registry));

    let mut names: HashSet<String> = HashSet::new();
    for token in requested {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        if registry.get_tool(trimmed).is_some() {
            names.insert(trimmed.to_string());
            continue;
        }
        match manager.resolve_toolset(trimmed) {
            Ok(resolved) => {
                for name in resolved {
                    names.insert(name);
                }
            }
            Err(_) => {
                tracing::warn!(
                    "Unknown platform toolset/token '{}' for platform '{}'",
                    trimmed,
                    platform
                );
            }
        }
    }

    // Merge explicit tool toggles from config:
    // - `enabled` acts as additive allow-list entries.
    // - `disabled` always removes entries.
    for tool_name in &config.tools_config.enabled {
        let trimmed = tool_name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if registry.get_tool(trimmed).is_some() {
            names.insert(trimmed.to_string());
        }
    }
    for tool_name in &config.tools_config.disabled {
        let trimmed = tool_name.trim();
        if trimmed.is_empty() {
            continue;
        }
        names.remove(trimmed);
    }

    let mut out: Vec<String> = names.into_iter().collect();
    out.sort();
    out
}

/// Resolve and filter tool schemas to those allowed for the given platform.
pub fn resolve_platform_tool_schemas(
    config: &GatewayConfig,
    platform: &str,
    registry: &Arc<ToolRegistry>,
) -> Vec<ToolSchema> {
    let all_defs = registry.get_definitions();
    let allowed = resolve_platform_tool_names(config, platform, registry);
    if allowed.is_empty() {
        return all_defs;
    }
    let allowed_set: HashSet<String> = allowed.into_iter().collect();
    let filtered: Vec<ToolSchema> = all_defs
        .iter()
        .filter(|schema| allowed_set.contains(&schema.name))
        .cloned()
        .collect();
    if filtered.is_empty() {
        return all_defs;
    }
    filtered
}

/// Compact tool-definition summary for hooks/transcript metadata.
pub fn tool_definition_summary(defs: &[ToolSchema]) -> Vec<serde_json::Value> {
    defs.iter()
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "description": d.description
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use async_trait::async_trait;
    use hermes_core::{tool_schema, JsonSchema, ToolError};

    struct NoopTool {
        schema: ToolSchema,
    }

    #[async_trait]
    impl hermes_core::ToolHandler for NoopTool {
        async fn execute(&self, _params: serde_json::Value) -> Result<String, ToolError> {
            Ok("ok".to_string())
        }

        fn schema(&self) -> ToolSchema {
            self.schema.clone()
        }
    }

    fn registry_with_minimal_tools() -> Arc<ToolRegistry> {
        let reg = Arc::new(ToolRegistry::new());
        let register = |reg: &Arc<ToolRegistry>, name: &str, toolset: &str| {
            let schema = tool_schema(name, &format!("{name} tool"), JsonSchema::new("object"));
            let handler = Arc::new(NoopTool {
                schema: schema.clone(),
            });
            reg.register(
                name,
                toolset,
                schema,
                handler,
                Arc::new(|| true),
                Vec::new(),
                true,
                format!("{name} tool"),
                "x",
                None,
            );
        };
        register(&reg, "web_search", "web");
        register(&reg, "terminal", "terminal");
        register(&reg, "process", "terminal");
        register(&reg, "read_file", "file");
        register(&reg, "write_file", "file");
        register(&reg, "patch", "file");
        register(&reg, "search_files", "file");
        register(&reg, "send_message", "messaging");
        register(&reg, "skills_list", "skills");
        register(&reg, "skill_view", "skills");
        register(&reg, "skill_manage", "skills");
        register(&reg, "memory", "memory");
        register(&reg, "todo", "todo");
        register(&reg, "clarify", "clarify");
        register(&reg, "browser_navigate", "browser");
        register(&reg, "browser_snapshot", "browser");
        register(&reg, "browser_click", "browser");
        register(&reg, "browser_type", "browser");
        register(&reg, "browser_scroll", "browser");
        register(&reg, "browser_back", "browser");
        register(&reg, "browser_press", "browser");
        register(&reg, "browser_get_images", "browser");
        register(&reg, "browser_vision", "browser");
        register(&reg, "browser_console", "browser");
        register(&reg, "vision_analyze", "vision");
        register(&reg, "execute_code", "code_execution");
        register(&reg, "delegate_task", "delegation");
        reg
    }

    #[test]
    fn normalize_platform_aliases() {
        assert_eq!(normalize_platform_key("local"), "cli");
        assert_eq!(normalize_platform_key("TG"), "telegram");
        assert_eq!(normalize_platform_key("discord"), "discord");
    }

    #[test]
    fn config_override_is_used_when_present() {
        let mut cfg = GatewayConfig::default();
        cfg.platform_toolsets
            .insert("cli".to_string(), vec!["web".to_string()]);
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "cli", &reg);
        assert!(names.contains(&"web_search".to_string()));
        assert!(!names.contains(&"terminal".to_string()));
    }

    #[test]
    fn platform_defaults_resolve_preset() {
        let cfg = GatewayConfig::default();
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "discord", &reg);
        assert!(names.contains(&"send_message".to_string()));
        assert!(names.contains(&"terminal".to_string()));
    }

    #[test]
    fn tools_config_enabled_adds_tools_outside_platform_toolset() {
        let mut cfg = GatewayConfig::default();
        cfg.platform_toolsets
            .insert("cli".to_string(), vec!["web".to_string()]);
        cfg.tools_config.enabled = vec!["terminal".to_string()];
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "cli", &reg);
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"terminal".to_string()));
    }

    #[test]
    fn tools_config_disabled_removes_even_if_platform_enables() {
        let mut cfg = GatewayConfig::default();
        cfg.platform_toolsets
            .insert("cli".to_string(), vec!["hermes-cli".to_string()]);
        cfg.tools_config.disabled = vec!["terminal".to_string()];
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "cli", &reg);
        assert!(!names.contains(&"terminal".to_string()));
    }
}
