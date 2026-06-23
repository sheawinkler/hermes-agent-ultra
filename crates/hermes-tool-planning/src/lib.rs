//! Runtime tool-planning helpers for resolving per-platform toolset configuration.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hermes_agent::coding_context::coding_toolset_selection;
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

/// Runtime defaults when no explicit `platform_toolsets` entry exists.
pub fn default_platform_toolsets() -> HashMap<String, Vec<String>> {
    let mut map = hermes_config::config::default_platform_toolsets();
    map.entry("api_server".to_string())
        .or_insert_with(|| vec!["hermes-api-server".to_string()]);
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

fn canonical_toolset_token(token: &str) -> String {
    let mut token = token.trim().to_ascii_lowercase();
    if let Some(stripped) = token
        .strip_suffix("_tools")
        .or_else(|| token.strip_suffix("-tools"))
    {
        token = stripped.to_string();
    }
    match token.as_str() {
        "image-gen" | "imagegen" => "image_gen".to_string(),
        "video-gen" | "videogen" => "video_gen".to_string(),
        "code-execution" | "code" => "code_execution".to_string(),
        "session-search" => "session_search".to_string(),
        "home-assistant" | "home_assistant" | "ha" => "homeassistant".to_string(),
        "mixture-of-agents" | "mixture-of-agent" | "mixture" | "moa" => {
            "mixture_of_agents".to_string()
        }
        "browser-use" | "browser_use" => "browser".to_string(),
        "voice-mode" | "voice_mode" => "voice".to_string(),
        "hermes_cli" => "hermes-cli".to_string(),
        "hermes_acp" => "hermes-acp".to_string(),
        "hermes_api_server" => "hermes-api-server".to_string(),
        "hermes_cron" => "hermes-cron".to_string(),
        "hermes_telegram" => "hermes-telegram".to_string(),
        "hermes_discord" => "hermes-discord".to_string(),
        "hermes_whatsapp" => "hermes-whatsapp".to_string(),
        "hermes_slack" => "hermes-slack".to_string(),
        _ => token,
    }
}

fn platform_has_custom_toolsets(config: &GatewayConfig, key: &str) -> bool {
    let Some(configured) = config.platform_toolsets.get(key) else {
        return false;
    };
    if configured.is_empty() {
        return false;
    }
    match default_platform_toolsets().get(key) {
        Some(defaults) => configured != defaults,
        None => true,
    }
}

fn coding_focus_toolsets(
    config: &GatewayConfig,
    platform: &str,
    manager: &ToolsetManager,
) -> Option<Vec<String>> {
    let key = normalize_platform_key(platform);
    if platform_has_custom_toolsets(config, &key) {
        return None;
    }
    coding_toolset_selection(Some(&key), None, Some(&config.agent.coding_context))?;

    let mut requested = vec!["coding".to_string()];
    let mut live_mcp: Vec<String> = manager
        .list_toolsets()
        .into_iter()
        .filter(|name| name.starts_with("mcp-"))
        .collect();
    live_mcp.sort();
    for name in live_mcp {
        if !requested.contains(&name) {
            requested.push(name);
        }
    }
    Some(requested)
}

/// Resolve tool names allowed for this platform based on configured toolsets.
pub fn resolve_platform_tool_names(
    config: &GatewayConfig,
    platform: &str,
    registry: &Arc<ToolRegistry>,
) -> Vec<String> {
    let manager = ToolsetManager::new(Arc::clone(registry));
    let requested = coding_focus_toolsets(config, platform, &manager)
        .unwrap_or_else(|| configured_platform_toolsets(config, platform));

    let mut names: HashSet<String> = HashSet::new();
    for token in requested {
        let original = token.trim();
        if original.is_empty() {
            continue;
        }
        let canonical = canonical_toolset_token(original);
        let candidates = if canonical == original {
            vec![canonical.as_str()]
        } else {
            vec![canonical.as_str(), original]
        };
        let mut matched = false;
        for candidate in candidates {
            if let Ok(resolved) = manager.resolve_toolset(candidate) {
                for name in resolved {
                    names.insert(name);
                }
                matched = true;
                break;
            }
            if registry.get_tool(candidate).is_some() {
                names.insert(candidate.to_string());
                matched = true;
                break;
            }
        }
        if !matched {
            tracing::warn!(
                "Unknown platform toolset/token '{}' for platform '{}'",
                original,
                platform
            );
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

    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use async_trait::async_trait;
    use hermes_core::{tool_schema, JsonSchema, ToolError};

    struct NoopTool {
        schema: ToolSchema,
    }

    fn env_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
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
        register(&reg, "web_extract", "web");
        register(&reg, "web_crawl", "web");
        register(&reg, "terminal", "terminal");
        register(&reg, "process", "terminal");
        register(&reg, "read_file", "file");
        register(&reg, "write_file", "file");
        register(&reg, "patch", "file");
        register(&reg, "search_files", "file");
        register(&reg, "vision_analyze", "vision");
        register(&reg, "image_generate", "image_gen");
        register(&reg, "execute_code", "code_execution");
        register(&reg, "delegate_task", "delegation");
        register(&reg, "session_search", "session_search");
        register(&reg, "cronjob", "cronjob");
        register(&reg, "ha_list_entities", "homeassistant");
        register(&reg, "ha_get_state", "homeassistant");
        register(&reg, "ha_list_services", "homeassistant");
        register(&reg, "ha_call_service", "homeassistant");
        register(&reg, "text_to_speech", "tts");
        register(&reg, "send_message", "messaging");
        register(&reg, "skills_list", "skills");
        register(&reg, "skill_view", "skills");
        register(&reg, "skill_manage", "skills");
        register(&reg, "memory", "memory");
        register(&reg, "todo", "todo");
        register(&reg, "clarify", "clarify");
        register(&reg, "auth_snapshot", "system");
        register(&reg, "integrations_snapshot", "system");
        register(&reg, "objective_snapshot", "system");
        register(&reg, "mission_snapshot", "system");
        register(&reg, "ops_snapshot", "system");
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
        register(&reg, "browser_cdp", "browser");
        register(&reg, "browser_dialog", "browser");
        register(&reg, "video_analyze", "vision");
        register(&reg, "video_generate", "video_gen");
        register(&reg, "mixture_of_agents", "mixture_of_agents");
        register(&reg, "transcription", "voice");
        register(&reg, "voice_mode", "voice");
        register(&reg, "process_registry", "terminal");
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
        assert!(names.contains(&"web_crawl".to_string()));
        assert!(!names.contains(&"terminal".to_string()));
    }

    #[test]
    fn platform_defaults_resolve_preset() {
        let cfg = GatewayConfig::default();
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "discord", &reg);
        assert!(names.contains(&"send_message".to_string()));
        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"integrations_snapshot".to_string()));
        assert!(names.contains(&"objective_snapshot".to_string()));
        assert!(names.contains(&"mission_snapshot".to_string()));
    }

    #[test]
    fn runtime_defaults_inherit_config_defaults_and_extend_api_server() {
        let defaults = default_platform_toolsets();
        assert_eq!(
            defaults.get("cron").cloned().unwrap_or_default(),
            vec!["hermes-cron".to_string()]
        );
        assert_eq!(
            defaults.get("api_server").cloned().unwrap_or_default(),
            vec!["hermes-api-server".to_string()]
        );

        let cfg = GatewayConfig::default();
        assert_eq!(
            configured_platform_toolsets(&cfg, "cron"),
            vec!["hermes-cron".to_string()]
        );
    }

    #[test]
    fn acp_default_resolves_editor_toolset() {
        let cfg = GatewayConfig::default();
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "acp", &reg);
        for expected in [
            "terminal",
            "read_file",
            "write_file",
            "patch",
            "search_files",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "acp should include {expected}"
            );
        }
        assert!(!names.contains(&"send_message".to_string()));
    }

    #[test]
    fn api_server_defaults_to_restricted_toolset() {
        let cfg = GatewayConfig::default();
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "api_server", &reg);
        for expected in [
            "web_search",
            "web_extract",
            "web_crawl",
            "terminal",
            "process",
            "read_file",
            "write_file",
            "patch",
            "search_files",
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_scroll",
            "browser_back",
            "browser_press",
            "vision_analyze",
            "image_generate",
            "execute_code",
            "delegate_task",
            "todo",
            "memory",
            "session_search",
            "cronjob",
            "ha_list_entities",
            "ha_get_state",
            "ha_list_services",
            "ha_call_service",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "api_server should include {expected}"
            );
        }
        for excluded in ["clarify", "send_message", "text_to_speech"] {
            assert!(
                !names.contains(&excluded.to_string()),
                "api_server should exclude {excluded}"
            );
        }
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

    #[test]
    fn coding_focus_collapses_default_cli_toolset_and_keeps_live_mcp() {
        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        let _cwd = EnvGuard::set("TERMINAL_CWD", tmp.path().to_str().unwrap());
        let mut cfg = GatewayConfig::default();
        cfg.agent.coding_context = "focus".to_string();
        let reg = registry_with_minimal_tools();
        let schema = tool_schema(
            "mcp_lattice_search",
            "ContextLattice search",
            JsonSchema::new("object"),
        );
        reg.register(
            "mcp_lattice_search",
            "mcp-lattice",
            schema.clone(),
            Arc::new(NoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "ContextLattice search",
            "x",
            None,
        );

        let names = resolve_platform_tool_names(&cfg, "cli", &reg);
        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"mcp_lattice_search".to_string()));
        assert!(!names.contains(&"send_message".to_string()));
        assert!(!names.contains(&"image_generate".to_string()));
    }

    #[test]
    fn coding_auto_is_prompt_only_and_custom_toolset_wins_over_focus() {
        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        let _cwd = EnvGuard::set("TERMINAL_CWD", tmp.path().to_str().unwrap());
        let reg = registry_with_minimal_tools();

        let mut auto_cfg = GatewayConfig::default();
        auto_cfg.agent.coding_context = "auto".to_string();
        let auto_names = resolve_platform_tool_names(&auto_cfg, "cli", &reg);
        assert!(auto_names.contains(&"send_message".to_string()));
        assert!(auto_names.contains(&"image_generate".to_string()));

        let mut custom_focus = GatewayConfig::default();
        custom_focus.agent.coding_context = "focus".to_string();
        custom_focus
            .platform_toolsets
            .insert("cli".to_string(), vec!["web".to_string()]);
        let custom_names = resolve_platform_tool_names(&custom_focus, "cli", &reg);
        assert!(custom_names.contains(&"web_search".to_string()));
        assert!(!custom_names.contains(&"terminal".to_string()));
    }

    #[test]
    fn platform_toolsets_all_and_star_aliases_expand_to_available_tools() {
        let mut cfg = GatewayConfig::default();
        let reg = registry_with_minimal_tools();

        cfg.platform_toolsets
            .insert("cli".to_string(), vec!["all".to_string()]);
        let all_names = resolve_platform_tool_names(&cfg, "cli", &reg);
        for expected in [
            "browser_console",
            "image_generate",
            "mixture_of_agents",
            "video_generate",
            "voice_mode",
        ] {
            assert!(
                all_names.contains(&expected.to_string()),
                "all alias should include {expected}"
            );
        }

        cfg.platform_toolsets
            .insert("cli".to_string(), vec!["*".to_string()]);
        let star_names = resolve_platform_tool_names(&cfg, "cli", &reg);
        assert_eq!(all_names, star_names);
    }

    #[test]
    fn platform_toolset_tokens_accept_common_alias_spellings() {
        let mut cfg = GatewayConfig::default();
        cfg.platform_toolsets.insert(
            "cli".to_string(),
            vec![
                "Image-Gen".to_string(),
                "MOA".to_string(),
                "voice_mode".to_string(),
                "home-assistant".to_string(),
                "Terminal".to_string(),
            ],
        );
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "cli", &reg);

        for expected in [
            "image_generate",
            "mixture_of_agents",
            "transcription",
            "voice_mode",
            "ha_call_service",
            "terminal",
            "process",
            "process_registry",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "alias resolution should include {expected}"
            );
        }
    }

    #[test]
    fn platform_toolset_tokens_strip_legacy_tools_suffix() {
        let mut cfg = GatewayConfig::default();
        cfg.platform_toolsets.insert(
            "cli".to_string(),
            vec![
                "homeassistant_tools".to_string(),
                "web_tools".to_string(),
                "terminal_tools".to_string(),
            ],
        );
        let reg = registry_with_minimal_tools();
        let names = resolve_platform_tool_names(&cfg, "cli", &reg);

        for expected in ["ha_call_service", "web_search", "terminal"] {
            assert!(
                names.contains(&expected.to_string()),
                "legacy _tools suffix should resolve {expected}"
            );
        }
    }

    #[test]
    fn platform_toolset_aliases_preserve_exact_custom_tool_fallback() {
        let mut cfg = GatewayConfig::default();
        cfg.platform_toolsets
            .insert("cli".to_string(), vec!["CustomTool".to_string()]);
        let reg = registry_with_minimal_tools();
        let schema = tool_schema("CustomTool", "custom tool", JsonSchema::new("object"));
        reg.register(
            "CustomTool",
            "custom",
            schema.clone(),
            Arc::new(NoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "custom tool",
            "x",
            None,
        );

        let names = resolve_platform_tool_names(&cfg, "cli", &reg);
        assert!(names.contains(&"CustomTool".to_string()));
    }

    #[test]
    fn platform_toolsets_resolve_live_mcp_server_aliases() {
        let mut cfg = GatewayConfig::default();
        cfg.platform_toolsets
            .insert("cli".to_string(), vec!["dynserver".to_string()]);
        let reg = registry_with_minimal_tools();
        let schema = tool_schema(
            "mcp_dynserver_ping",
            "MCP server ping",
            JsonSchema::new("object"),
        );
        reg.register(
            "mcp_dynserver_ping",
            "mcp-dynserver",
            schema.clone(),
            Arc::new(NoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "MCP server ping",
            "x",
            None,
        );
        reg.register_toolset_alias("dynserver", "mcp-dynserver");

        let names = resolve_platform_tool_names(&cfg, "cli", &reg);
        assert_eq!(names, vec!["mcp_dynserver_ping".to_string()]);
    }
}
