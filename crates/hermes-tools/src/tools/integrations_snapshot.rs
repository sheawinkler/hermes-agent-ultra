//! Read-only integration control-plane snapshots.
//!
//! The CLI exposes `/integrations status|all|auth|providers|gateway|memory|repair|snapshot`.
//! This tool makes the same diagnostics available to agents as structured JSON
//! without writing snapshot files or mutating auth, gateway, plugin, or memory state.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use hermes_core::providers::{known_providers, provider_capability_for};
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tools::auth_snapshot::auth_status_snapshot;
use crate::ToolRegistry;

const TOOL_NAME: &str = "integrations_snapshot";
const DEFAULT_CONTEXTLATTICE_URL: &str = "http://127.0.0.1:8075";

#[derive(Clone)]
pub struct IntegrationsSnapshotHandler {
    registry: Arc<ToolRegistry>,
}

impl IntegrationsSnapshotHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ToolHandler for IntegrationsSnapshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();
        let provider = params
            .get("provider")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let model = params
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let memory_url = params
            .get("memory_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(contextlattice_url);
        let timeout_ms = bounded_timeout_ms(params.get("timeout_ms"));
        let probe_memory = params
            .get("probe_memory")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let include_plugins = params
            .get("include_plugins")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let payload = match action.as_str() {
            "status" | "all" | "snapshot" => {
                integration_snapshot(
                    &self.registry,
                    provider,
                    model,
                    &memory_url,
                    timeout_ms,
                    probe_memory,
                    include_plugins,
                )
                .await
            }
            "auth" => {
                let auth = auth_panel(provider, model);
                json!({
                    "status": "ok",
                    "action": action,
                    "captured_at": Utc::now().to_rfc3339(),
                    "auth": auth,
                    "mutable": false,
                })
            }
            "providers" => json!({
                "status": "ok",
                "action": action,
                "captured_at": Utc::now().to_rfc3339(),
                "providers": providers_panel(),
                "mutable": false,
            }),
            "gateway" => json!({
                "status": "ok",
                "action": action,
                "captured_at": Utc::now().to_rfc3339(),
                "gateway": gateway_panel(&self.registry, include_plugins),
                "mutable": false,
            }),
            "memory" => json!({
                "status": "ok",
                "action": action,
                "captured_at": Utc::now().to_rfc3339(),
                "memory": memory_panel(&memory_url, timeout_ms, probe_memory).await,
                "mutable": false,
            }),
            "repair" => {
                let auth = auth_panel(provider, model);
                let memory = memory_panel(&memory_url, timeout_ms, probe_memory).await;
                json!({
                    "status": "ok",
                    "action": action,
                    "captured_at": Utc::now().to_rfc3339(),
                    "repair": repair_plan(&auth, &memory),
                    "mutable": false,
                })
            }
            "help" => json!({
                "status": "ok",
                "tool": TOOL_NAME,
                "actions": ["status", "all", "auth", "providers", "gateway", "memory", "repair", "snapshot", "help"],
                "notes": [
                    "read-only integration control-plane snapshot",
                    "does not write snapshot files; use the returned JSON as the snapshot artifact",
                    "does not refresh credentials, enable adapters, mutate plugins, or write memory"
                ],
            }),
            _ => {
                return Err(ToolError::InvalidParams(format!(
                    "unknown action '{action}'; expected status|all|auth|providers|gateway|memory|repair|snapshot|help"
                )));
            }
        };

        Ok(payload.to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "all", "auth", "providers", "gateway", "memory", "repair", "snapshot", "help"],
                "description": "Snapshot action. Defaults to status."
            }),
        );
        props.insert(
            "provider".into(),
            json!({
                "type": "string",
                "description": "Optional provider override for auth diagnostics."
            }),
        );
        props.insert(
            "model".into(),
            json!({
                "type": "string",
                "description": "Optional provider:model hint used when provider is omitted."
            }),
        );
        props.insert(
            "memory_url".into(),
            json!({
                "type": "string",
                "description": "ContextLattice orchestrator URL. Defaults to env or http://127.0.0.1:8075."
            }),
        );
        props.insert(
            "timeout_ms".into(),
            json!({
                "type": "integer",
                "minimum": 100,
                "maximum": 10000,
                "description": "ContextLattice health probe timeout in milliseconds. Defaults to 2000."
            }),
        );
        props.insert(
            "probe_memory".into(),
            json!({
                "type": "boolean",
                "description": "Run ContextLattice /health probe. Defaults to true; set false for offline snapshots."
            }),
        );
        props.insert(
            "include_plugins".into(),
            json!({
                "type": "boolean",
                "description": "Include local plugin manifest counts and rows. Defaults to true."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "Return read-only auth, provider, gateway, plugin, and ContextLattice integration diagnostics.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub async fn integration_snapshot(
    registry: &ToolRegistry,
    provider: Option<&str>,
    model: Option<&str>,
    memory_url: &str,
    timeout_ms: u64,
    probe_memory: bool,
    include_plugins: bool,
) -> Value {
    integration_snapshot_from_request(IntegrationSnapshotRequest {
        registry,
        provider,
        model,
        memory_url,
        timeout_ms,
        probe_memory,
        include_plugins,
        config_path_override: None,
        auth_override: None,
    })
    .await
}

struct IntegrationSnapshotRequest<'a> {
    registry: &'a ToolRegistry,
    provider: Option<&'a str>,
    model: Option<&'a str>,
    memory_url: &'a str,
    timeout_ms: u64,
    probe_memory: bool,
    include_plugins: bool,
    config_path_override: Option<&'a Path>,
    auth_override: Option<Value>,
}

async fn integration_snapshot_from_request(request: IntegrationSnapshotRequest<'_>) -> Value {
    let auth = request
        .auth_override
        .unwrap_or_else(|| auth_panel(request.provider, request.model));
    let memory = memory_panel(request.memory_url, request.timeout_ms, request.probe_memory).await;
    json!({
        "status": "ok",
        "action": "status",
        "captured_at": Utc::now().to_rfc3339(),
        "auth": auth,
        "providers": providers_panel(),
        "gateway": gateway_panel_with_config_path(
            request.registry,
            request.include_plugins,
            request.config_path_override,
        ),
        "memory": memory,
        "repair": repair_plan(&auth, &memory),
        "snapshot_file_written": false,
        "mutable": false,
        "secret_values_emitted": false,
    })
}

fn auth_panel(provider: Option<&str>, model: Option<&str>) -> Value {
    let snapshot = auth_status_snapshot(provider, model, false);
    let provider = snapshot
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let cap = provider_capability_for(provider);
    let oauth_supported = snapshot
        .get("oauth")
        .and_then(|oauth| oauth.get("supported"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let credential_present = snapshot
        .get("credential")
        .and_then(|credential| credential.get("present"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let oauth_state_present = snapshot
        .get("auth_store")
        .and_then(|store| store.get("present"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ok = credential_present || (oauth_supported && oauth_state_present);

    json!({
        "provider": provider,
        "model_hint": snapshot.get("model_hint").cloned().unwrap_or(Value::Null),
        "oauth_capable": oauth_supported,
        "managed_tools_supported": cap.as_ref().map(|cap| cap.managed_tools_supported).unwrap_or(false),
        "credential_present": credential_present,
        "oauth_state_present": oauth_state_present,
        "status": if ok { "PASS" } else { "FAIL" },
        "credential": snapshot.get("credential").cloned().unwrap_or_else(|| json!({})),
        "auth_store": snapshot.get("auth_store").cloned().unwrap_or_else(|| json!({})),
        "oauth_runtime_gate": snapshot.get("oauth").and_then(|oauth| oauth.get("gate")).cloned(),
        "secret_values_emitted": false,
    })
}

fn providers_panel() -> Value {
    let mut seen = BTreeSet::new();
    let providers = known_providers()
        .into_iter()
        .filter_map(|provider| {
            let cap = provider_capability_for(provider)?;
            if !seen.insert(cap.id.clone()) {
                return None;
            }
            Some(json!({
                "provider": cap.id,
                "oauth_supported": cap.oauth_supported,
                "models_dev_merged": cap.models_dev_merged,
                "managed_tools_supported": cap.managed_tools_supported,
            }))
        })
        .collect::<Vec<_>>();

    json!({
        "configured_providers": providers
            .iter()
            .filter_map(|provider| provider.get("provider").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        "provider_count": providers.len(),
        "providers": providers,
    })
}

fn gateway_panel(registry: &ToolRegistry, include_plugins: bool) -> Value {
    gateway_panel_with_config_path(registry, include_plugins, None)
}

fn gateway_panel_with_config_path(
    registry: &ToolRegistry,
    include_plugins: bool,
    config_path_override: Option<&Path>,
) -> Value {
    let config_path = config_path_override
        .map(PathBuf::from)
        .unwrap_or_else(hermes_config::config_path);
    let (config_loaded, config_error, config) =
        match hermes_config::load_user_config_file(&config_path) {
            Ok(config) => (true, None, config),
            Err(err) => (
                false,
                Some(err.to_string()),
                hermes_config::GatewayConfig::default(),
            ),
        };
    let plugin_rows = if include_plugins {
        discover_plugin_surface()
    } else {
        Vec::new()
    };
    let enabled_plugins = plugin_rows.iter().filter(|row| row.enabled).count();

    json!({
        "config_path": config_path.display().to_string(),
        "config_loaded": config_loaded,
        "config_error": config_error,
        "platform_adapters": config.platforms.len(),
        "enabled_platform_adapters": config.platforms.values().filter(|platform| platform.enabled).count(),
        "mcp_servers": config.mcp_servers.len(),
        "toolsets": config.platform_toolsets.len(),
        "registered_tools": registry.list_tools().len(),
        "plugins": {
            "included": include_plugins,
            "count": plugin_rows.len(),
            "enabled": enabled_plugins,
            "rows": plugin_rows,
        },
    })
}

async fn memory_panel(memory_url: &str, timeout_ms: u64, probe_memory: bool) -> Value {
    let normalized = memory_url.trim_end_matches('/').to_string();
    let health_url = format!("{normalized}/health");
    if !probe_memory {
        return json!({
            "contextlattice_url": normalized,
            "health_url": health_url,
            "probe": "SKIPPED",
            "status": "skipped",
        });
    }

    let timeout = Duration::from_millis(timeout_ms);
    match reqwest::Client::builder().timeout(timeout).build() {
        Ok(client) => match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => json!({
                "contextlattice_url": normalized,
                "health_url": health_url,
                "probe": "PASS",
                "status": "pass",
                "http_status": resp.status().as_u16(),
            }),
            Ok(resp) => json!({
                "contextlattice_url": normalized,
                "health_url": health_url,
                "probe": "WARN",
                "status": "warn",
                "http_status": resp.status().as_u16(),
            }),
            Err(err) => json!({
                "contextlattice_url": normalized,
                "health_url": health_url,
                "probe": "WARN",
                "status": "warn",
                "error": truncate_chars(&err.to_string(), 160),
            }),
        },
        Err(err) => json!({
            "contextlattice_url": normalized,
            "health_url": health_url,
            "probe": "WARN",
            "status": "warn",
            "error": format!("client build failed: {}", truncate_chars(&err.to_string(), 160)),
        }),
    }
}

fn repair_plan(auth: &Value, memory: &Value) -> Value {
    let auth_ok = auth.get("status").and_then(Value::as_str) == Some("PASS");
    let memory_ok = memory.get("status").and_then(Value::as_str) == Some("pass")
        || memory.get("status").and_then(Value::as_str) == Some("skipped");
    let mut steps = Vec::new();
    if auth_ok {
        steps.push(json!({"area": "auth", "status": "PASS"}));
    } else {
        steps.push(json!({
            "area": "auth",
            "status": "FAIL",
            "next": "run `/auth status` then `/auth verify` or `hermes-ultra auth add`"
        }));
    }
    if memory_ok {
        steps.push(json!({"area": "contextlattice", "status": memory.get("status").cloned().unwrap_or_else(|| json!("unknown"))}));
    } else {
        steps.push(json!({
            "area": "contextlattice",
            "status": "WARN",
            "next": "verify local orchestrator and CONTEXTLATTICE_ORCHESTRATOR_URL/MEMMCP_ORCHESTRATOR_URL"
        }));
    }
    steps.push(json!({
        "area": "tools",
        "status": "CHECK",
        "next": "run `tools list`, `ops_snapshot`, or `/integrations status` for registry health"
    }));

    json!({
        "overall": if auth_ok && memory_ok { "PASS" } else { "ACTION_REQUIRED" },
        "steps": steps,
    })
}

fn contextlattice_url() -> String {
    env_nonempty("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|| env_nonempty("MEMMCP_ORCHESTRATOR_URL"))
        .unwrap_or_else(|| DEFAULT_CONTEXTLATTICE_URL.to_string())
}

fn bounded_timeout_ms(input: Option<&Value>) -> u64 {
    input
        .and_then(Value::as_u64)
        .unwrap_or(2_000)
        .clamp(100, 10_000)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Clone, serde::Serialize)]
struct PluginSurfaceEntry {
    name: String,
    version: String,
    description: String,
    kind: Option<String>,
    source: String,
    path: String,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct PluginManifest {
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    kind: Option<String>,
}

fn discover_plugin_surface() -> Vec<PluginSurfaceEntry> {
    let mut rows = Vec::new();
    rows.extend(scan_plugin_manifest_root(
        &hermes_config::hermes_home().join("plugins"),
        "user",
    ));
    if hermes_config::env_var_enabled("HERMES_ENABLE_PROJECT_PLUGINS") {
        if let Ok(cwd) = std::env::current_dir() {
            rows.extend(scan_plugin_manifest_root(
                &cwd.join(".hermes").join("plugins"),
                "project",
            ));
        }
    }
    rows.sort_by(|a, b| {
        a.source.cmp(&b.source).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    rows
}

fn scan_plugin_manifest_root(root: &Path, source: &str) -> Vec<PluginSurfaceEntry> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let manifest_path = path.join("plugin.yaml");
            let raw = std::fs::read_to_string(&manifest_path).ok()?;
            let manifest = serde_yaml::from_str::<PluginManifest>(&raw).ok()?;
            Some(PluginSurfaceEntry {
                name: manifest.name,
                version: manifest.version,
                description: manifest.description,
                kind: coerce_memory_provider_kind(&path, manifest.kind),
                source: source.to_string(),
                path: path.display().to_string(),
                enabled: !path.join(".disabled").exists(),
            })
        })
        .collect()
}

fn coerce_memory_provider_kind(path: &Path, kind: Option<String>) -> Option<String> {
    let explicit = kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if explicit.is_some() {
        return explicit;
    }
    let Ok(source) = std::fs::read_to_string(path.join("__init__.py")) else {
        return None;
    };
    let probe = source.get(..8192).unwrap_or(source.as_str());
    if probe.contains("register_memory_provider") || probe.contains("MemoryProvider") {
        Some("exclusive".to_string())
    } else {
        None
    }
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let mut out = input
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use hermes_core::{AgentError, CommandOutput, Skill, SkillMeta, TerminalBackend};
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    use super::*;

    async fn env_lock() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().await
    }

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.old {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[derive(Default)]
    struct NoopTerminalBackend;

    #[async_trait]
    impl TerminalBackend for NoopTerminalBackend {
        async fn execute_command(
            &self,
            _command: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _background: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }

        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }

        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(false)
        }

        async fn list_processes(&self) -> Result<Value, AgentError> {
            Ok(json!([]))
        }
    }

    struct NoopSkillProvider;

    #[async_trait]
    impl hermes_core::SkillProvider for NoopSkillProvider {
        async fn create_skill(
            &self,
            name: &str,
            content: &str,
            category: Option<&str>,
        ) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: category.map(String::from),
                description: None,
            })
        }

        async fn get_skill(&self, _name: &str) -> Result<Option<Skill>, AgentError> {
            Ok(None)
        }

        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(Vec::new())
        }

        async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: None,
                description: None,
            })
        }

        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    fn registry_with_tool() -> ToolRegistry {
        let registry = ToolRegistry::new();
        crate::register_builtins::register_builtin_tools(
            &registry,
            Arc::new(NoopTerminalBackend),
            Arc::new(NoopSkillProvider),
        );
        registry
    }

    #[tokio::test]
    async fn status_snapshot_is_read_only_and_reports_core_panels() {
        let _lock = env_lock().await;
        let home = tempdir().expect("home");
        let config_path = home.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "platforms:\n  telegram:\n    enabled: true\nmcp_servers:\n  - name: lattice\n    command: contextlattice\n",
        )
        .expect("write config");

        let registry = registry_with_tool();
        let auth = json!({
            "provider": "openrouter",
            "model_hint": Value::Null,
            "oauth_capable": false,
            "managed_tools_supported": false,
            "credential_present": true,
            "oauth_state_present": false,
            "status": "PASS",
            "credential": {
                "present": true,
                "env_keys": ["OPENROUTER_API_KEY"],
            },
            "auth_store": {
                "present": false,
            },
            "oauth_runtime_gate": Value::Null,
            "secret_values_emitted": false,
        });
        let payload = integration_snapshot_from_request(IntegrationSnapshotRequest {
            registry: &registry,
            provider: Some("openrouter"),
            model: None,
            memory_url: DEFAULT_CONTEXTLATTICE_URL,
            timeout_ms: 100,
            probe_memory: false,
            include_plugins: false,
            config_path_override: Some(&config_path),
            auth_override: Some(auth),
        })
        .await;
        let raw = payload.to_string();

        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["auth"]["provider"], "openrouter");
        assert_eq!(payload["auth"]["status"], "PASS");
        assert_eq!(payload["gateway"]["platform_adapters"], 1);
        assert_eq!(payload["gateway"]["enabled_platform_adapters"], 1);
        assert_eq!(payload["gateway"]["mcp_servers"], 1);
        assert_eq!(payload["memory"]["status"], "skipped");
        assert_eq!(payload["mutable"], false);
        assert_eq!(payload["secret_values_emitted"], false);
        assert!(!raw.contains("sk-"));
    }

    #[tokio::test]
    async fn repair_plan_flags_missing_auth_without_writing_snapshot_file() {
        let _lock = env_lock().await;
        let home = tempdir().expect("home");
        let _home = EnvGuard::set("HERMES_HOME", home.path().to_string_lossy().as_ref());
        let _provider = EnvGuard::set("HERMES_AUTH_DEFAULT_PROVIDER", "openrouter");
        let _key = EnvGuard::remove("OPENROUTER_API_KEY");
        let _legacy_key = EnvGuard::remove("HERMES_OPENAI_API_KEY");

        let registry = ToolRegistry::new();
        let handler = IntegrationsSnapshotHandler::new(Arc::new(registry));
        let raw = handler
            .execute(json!({
                "action": "repair",
                "probe_memory": false,
                "include_plugins": false
            }))
            .await
            .expect("execute");
        let payload: Value = serde_json::from_str(&raw).expect("json");

        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["repair"]["overall"], "ACTION_REQUIRED");
        assert_eq!(payload["mutable"], false);
        assert!(!home.path().join("logs").exists());
    }
}
