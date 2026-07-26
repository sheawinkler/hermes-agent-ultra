//! Configuration loading from YAML, JSON, and environment variables.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

// Re-export ConfigError for convenience
pub use hermes_core::ConfigError;

use hermes_core::{
    resolve_execution_mode, ExecutionMode, ExecutionModeSource, ResolvedExecutionMode,
};

use crate::config::{
    DisplayConfig, ExecutionModeConfigValue, GatewayConfig, LlmProviderConfig, ProxyConfig,
    TerminalBackendType, TerminalConfig, TerminalHomeMode, WebConfig,
};
use crate::merge::{deep_merge, merge_configs};
use crate::paths;

const MANAGED_SCOPE_ENV: &str = "HERMES_MANAGED_DIR";
pub const EXECUTION_MODE_ENV_KEY: &str = "HERMES_EXECUTION_MODE";
#[cfg(not(test))]
const DEFAULT_MANAGED_SCOPE_DIR: &str = "/etc/hermes";

/// Process-local last known good snapshots for a user-owned `config.yaml`.
///
/// A syntax or validation error in an already-loaded file may be transient
/// (for example, an editor writes the file in multiple passes). Retaining a
/// previously validated snapshot keeps a running process safe and usable,
/// while a first-load failure and all I/O failures remain fail-closed.
static LAST_KNOWN_GOOD_TYPED_CONFIGS: LazyLock<Mutex<HashMap<PathBuf, GatewayConfig>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LAST_KNOWN_GOOD_RAW_CONFIGS: LazyLock<Mutex<HashMap<PathBuf, serde_json::Value>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn config_cache_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn cache_last_known_good_typed_config(path: &Path, config: &GatewayConfig) {
    let cache = &*LAST_KNOWN_GOOD_TYPED_CONFIGS;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(config_cache_key(path), config.clone());
    }
}

fn last_known_good_typed_config(path: &Path) -> Option<GatewayConfig> {
    let cache = &*LAST_KNOWN_GOOD_TYPED_CONFIGS;
    cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(&config_cache_key(path)).cloned())
}

fn cache_last_known_good_raw_config(path: &Path, config: &serde_json::Value) {
    let cache = &*LAST_KNOWN_GOOD_RAW_CONFIGS;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(config_cache_key(path), config.clone());
    }
}

fn last_known_good_raw_config(path: &Path) -> Option<serde_json::Value> {
    let cache = &*LAST_KNOWN_GOOD_RAW_CONFIGS;
    cache
        .lock()
        .ok()
        .and_then(|cache| cache.get(&config_cache_key(path)).cloned())
}

fn retains_last_known_good_config(error: &ConfigError) -> bool {
    match error {
        ConfigError::ValidationError(_) => true,
        ConfigError::ParseError(message) => !message.starts_with("failed to read config file "),
        ConfigError::NotFound(_) | ConfigError::MergeConflict(_) => false,
    }
}

/// Controls whether a user-owned configuration document may retain legacy
/// unknown keys. Managed overlays are always strict and never use this option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserConfigLoadOptions {
    /// Preserve compatibility with legacy user config keys that this typed
    /// configuration model does not own.
    pub allow_unknown_keys: bool,
}

impl UserConfigLoadOptions {
    pub const fn strict() -> Self {
        Self {
            allow_unknown_keys: false,
        }
    }

    pub const fn compatibility() -> Self {
        Self {
            allow_unknown_keys: true,
        }
    }
}

impl Default for UserConfigLoadOptions {
    fn default() -> Self {
        Self::strict()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigDocumentStrictness {
    Strict,
    Compatibility,
}

impl ConfigDocumentStrictness {
    const fn from_user_options(options: UserConfigLoadOptions) -> Self {
        if options.allow_unknown_keys {
            Self::Compatibility
        } else {
            Self::Strict
        }
    }

    const fn rejects_unknown_keys(self) -> bool {
        matches!(self, Self::Strict)
    }
}

#[derive(Debug, Clone, Copy)]
enum ConfigKeySchema {
    Any,
    Object(&'static [(&'static str, ConfigKeySchema)]),
    ObjectWithExtras(&'static [(&'static str, ConfigKeySchema)]),
    Map(&'static ConfigKeySchema),
    Sequence(&'static ConfigKeySchema),
}

static ANY_CONFIG_KEY_SCHEMA: ConfigKeySchema = ConfigKeySchema::Any;
static PLATFORM_DISPLAY_CONFIG_KEY_SCHEMA: ConfigKeySchema =
    ConfigKeySchema::Object(PLATFORM_DISPLAY_CONFIG_KEYS);
static LLM_PROVIDER_CONFIG_KEY_SCHEMA: ConfigKeySchema =
    ConfigKeySchema::Object(LLM_PROVIDER_CONFIG_KEYS);
static AUXILIARY_TASK_CONFIG_KEY_SCHEMA: ConfigKeySchema =
    ConfigKeySchema::ObjectWithExtras(AUXILIARY_TASK_CONFIG_KEYS);
static QUICK_COMMAND_CONFIG_KEY_SCHEMA: ConfigKeySchema =
    ConfigKeySchema::Object(QUICK_COMMAND_CONFIG_KEYS);
static MCP_SERVER_CONFIG_KEY_SCHEMA: ConfigKeySchema =
    ConfigKeySchema::Object(MCP_SERVER_CONFIG_KEYS);
static ROOT_CONFIG_KEY_SCHEMA: ConfigKeySchema = ConfigKeySchema::Object(ROOT_CONFIG_KEYS);

static ROOT_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("model", ConfigKeySchema::Any),
    ("model_switch", ConfigKeySchema::Object(MODEL_SWITCH_CONFIG_KEYS)),
    ("personality", ConfigKeySchema::Any),
    ("max_turns", ConfigKeySchema::Any),
    ("execution_mode", ConfigKeySchema::Any),
    ("system_prompt", ConfigKeySchema::Any),
    ("prefill_messages_file", ConfigKeySchema::Any),
    ("tools", ConfigKeySchema::Any),
    ("budget", ConfigKeySchema::Object(BUDGET_CONFIG_KEYS)),
    ("resource_limits_enabled", ConfigKeySchema::Any),
    (
        "tool_output",
        ConfigKeySchema::Object(TOOL_OUTPUT_CONFIG_KEYS),
    ),
    ("platforms", ConfigKeySchema::Map(&ANY_CONFIG_KEY_SCHEMA)),
    (
        "platform_toolsets",
        ConfigKeySchema::Map(&ANY_CONFIG_KEY_SCHEMA),
    ),
    ("delegation", ConfigKeySchema::Object(DELEGATION_CONFIG_KEYS)),
    ("session", ConfigKeySchema::Object(SESSION_CONFIG_KEYS)),
    ("sessions", ConfigKeySchema::Object(SESSIONS_CONFIG_KEYS)),
    ("streaming", ConfigKeySchema::Object(STREAMING_CONFIG_KEYS)),
    ("display", ConfigKeySchema::Object(DISPLAY_CONFIG_KEYS)),
    ("terminal", ConfigKeySchema::Object(TERMINAL_CONFIG_KEYS)),
    ("web", ConfigKeySchema::Object(WEB_CONFIG_KEYS)),
    (
        "llm_providers",
        ConfigKeySchema::Map(&LLM_PROVIDER_CONFIG_KEY_SCHEMA),
    ),
    ("fallback_model", ConfigKeySchema::Any),
    ("fallback_models", ConfigKeySchema::Any),
    (
        "smart_model_routing",
        ConfigKeySchema::Object(SMART_MODEL_ROUTING_CONFIG_KEYS),
    ),
    (
        "auxiliary",
        ConfigKeySchema::Map(&AUXILIARY_TASK_CONFIG_KEY_SCHEMA),
    ),
    (
        "quick_commands",
        ConfigKeySchema::Map(&QUICK_COMMAND_CONFIG_KEY_SCHEMA),
    ),
    ("kanban", ConfigKeySchema::Object(KANBAN_CONFIG_KEYS)),
    ("tts", ConfigKeySchema::Any),
    ("proxy", ConfigKeySchema::Object(PROXY_CONFIG_KEYS)),
    ("approval", ConfigKeySchema::Object(APPROVAL_CONFIG_KEYS)),
    ("security", ConfigKeySchema::Object(SECURITY_CONFIG_KEYS)),
    ("skills", ConfigKeySchema::Object(SKILLS_CONFIG_KEYS)),
    ("tools_config", ConfigKeySchema::Object(TOOLS_CONFIG_KEYS)),
    (
        "mcp_servers",
        ConfigKeySchema::Sequence(&MCP_SERVER_CONFIG_KEY_SCHEMA),
    ),
    ("profile", ConfigKeySchema::Object(PROFILE_CONFIG_KEYS)),
    ("agent", ConfigKeySchema::Object(AGENT_CONFIG_KEYS)),
    ("home_dir", ConfigKeySchema::Any),
];

static MODEL_SWITCH_CONFIG_KEYS: &[(&str, ConfigKeySchema)] =
    &[("persist_switch_by_default", ConfigKeySchema::Any)];
static DISPLAY_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("tool_progress_command", ConfigKeySchema::Any),
    ("tool_progress", ConfigKeySchema::Any),
    ("busy_input_mode", ConfigKeySchema::Any),
    ("busy_ack_enabled", ConfigKeySchema::Any),
    ("memory_notifications", ConfigKeySchema::Any),
    ("friendly_tool_labels", ConfigKeySchema::Any),
    (
        "platforms",
        ConfigKeySchema::Map(&PLATFORM_DISPLAY_CONFIG_KEY_SCHEMA),
    ),
];
static PLATFORM_DISPLAY_CONFIG_KEYS: &[(&str, ConfigKeySchema)] =
    &[("tool_progress", ConfigKeySchema::Any)];
static TERMINAL_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("backend", ConfigKeySchema::Any),
    ("env_type", ConfigKeySchema::Any),
    ("timeout", ConfigKeySchema::Any),
    ("max_output_size", ConfigKeySchema::Any),
    ("workdir", ConfigKeySchema::Any),
    ("cwd", ConfigKeySchema::Any),
    ("docker_container_id", ConfigKeySchema::Any),
    ("docker_image", ConfigKeySchema::Any),
    ("docker_mount_cwd_to_workspace", ConfigKeySchema::Any),
    ("docker_run_as_host_user", ConfigKeySchema::Any),
    ("container_cpu", ConfigKeySchema::Any),
    ("container_memory", ConfigKeySchema::Any),
    ("container_disk", ConfigKeySchema::Any),
    ("container_persistent", ConfigKeySchema::Any),
    ("docker_env", ConfigKeySchema::Any),
    ("docker_forward_env", ConfigKeySchema::Any),
    ("docker_volumes", ConfigKeySchema::Any),
    ("vercel_runtime", ConfigKeySchema::Any),
    ("modal_mode", ConfigKeySchema::Any),
    ("shell_init_files", ConfigKeySchema::Any),
    ("auto_source_bashrc", ConfigKeySchema::Any),
    ("home_mode", ConfigKeySchema::Any),
    ("env_passthrough", ConfigKeySchema::Any),
    ("ssh_host", ConfigKeySchema::Any),
    ("ssh_port", ConfigKeySchema::Any),
    ("ssh_user", ConfigKeySchema::Any),
    ("ssh_key_path", ConfigKeySchema::Any),
];
static WEB_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("backend", ConfigKeySchema::Any),
    ("search_backend", ConfigKeySchema::Any),
    ("extract_backend", ConfigKeySchema::Any),
    ("crawl_backend", ConfigKeySchema::Any),
];
static LLM_PROVIDER_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("api_key", ConfigKeySchema::Any),
    ("api_key_env", ConfigKeySchema::Any),
    ("base_url", ConfigKeySchema::Any),
    ("request_timeout_seconds", ConfigKeySchema::Any),
    ("command", ConfigKeySchema::Any),
    ("args", ConfigKeySchema::Any),
    ("model", ConfigKeySchema::Any),
    ("models", ConfigKeySchema::Any),
    ("model_context_windows", ConfigKeySchema::Any),
    ("model_context_lengths", ConfigKeySchema::Any),
    ("discover_models", ConfigKeySchema::Any),
    ("api_mode", ConfigKeySchema::Any),
    ("max_tokens", ConfigKeySchema::Any),
    ("max_output_tokens", ConfigKeySchema::Any),
    ("temperature", ConfigKeySchema::Any),
    ("extra_body", ConfigKeySchema::Any),
    ("rate_limit", ConfigKeySchema::Any),
    ("credential_pool", ConfigKeySchema::Any),
    ("oauth_token_url", ConfigKeySchema::Any),
    ("oauth_client_id", ConfigKeySchema::Any),
];
static SMART_MODEL_ROUTING_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("enabled", ConfigKeySchema::Any),
    ("max_simple_chars", ConfigKeySchema::Any),
    ("max_simple_words", ConfigKeySchema::Any),
    (
        "cheap_model",
        ConfigKeySchema::Object(CHEAP_MODEL_ROUTE_CONFIG_KEYS),
    ),
];
static CHEAP_MODEL_ROUTE_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("provider", ConfigKeySchema::Any),
    ("model", ConfigKeySchema::Any),
    ("base_url", ConfigKeySchema::Any),
    ("api_key_env", ConfigKeySchema::Any),
];
static AUXILIARY_TASK_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("provider", ConfigKeySchema::Any),
    ("model", ConfigKeySchema::Any),
    ("base_url", ConfigKeySchema::Any),
    ("api_key", ConfigKeySchema::Any),
    ("timeout", ConfigKeySchema::Any),
    ("timeout_secs", ConfigKeySchema::Any),
    ("extra_body", ConfigKeySchema::Any),
    ("download_timeout", ConfigKeySchema::Any),
    ("extra", ConfigKeySchema::Any),
];
static QUICK_COMMAND_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("type", ConfigKeySchema::Any),
    ("kind", ConfigKeySchema::Any),
    ("command", ConfigKeySchema::Any),
    ("target", ConfigKeySchema::Any),
    ("timeout_secs", ConfigKeySchema::Any),
    ("timeout", ConfigKeySchema::Any),
];
static KANBAN_CONFIG_KEYS: &[(&str, ConfigKeySchema)] =
    &[("dispatch_in_gateway", ConfigKeySchema::Any)];
static APPROVAL_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("enabled", ConfigKeySchema::Any),
    ("dangerous_commands", ConfigKeySchema::Any),
    ("require_approval", ConfigKeySchema::Any),
    ("whitelist_commands", ConfigKeySchema::Any),
];
static SECURITY_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("allow_private_urls", ConfigKeySchema::Any),
    (
        "website_blocklist",
        ConfigKeySchema::Object(WEBSITE_BLOCKLIST_CONFIG_KEYS),
    ),
];
static WEBSITE_BLOCKLIST_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("enabled", ConfigKeySchema::Any),
    ("domains", ConfigKeySchema::Any),
    ("shared_files", ConfigKeySchema::Any),
];
static SKILLS_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("enabled", ConfigKeySchema::Any),
    ("disabled", ConfigKeySchema::Any),
];
static TOOLS_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("enabled", ConfigKeySchema::Any),
    ("disabled", ConfigKeySchema::Any),
    ("per_tool", ConfigKeySchema::Any),
];
static MCP_SERVER_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("name", ConfigKeySchema::Any),
    ("command", ConfigKeySchema::Any),
    ("url", ConfigKeySchema::Any),
    ("supports_parallel_tool_calls", ConfigKeySchema::Any),
    ("keepalive_interval", ConfigKeySchema::Any),
];
static PROFILE_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("current", ConfigKeySchema::Any),
    ("available", ConfigKeySchema::Any),
];
static AGENT_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    // `max_turns` is a recognized Python compatibility key that is lifted to
    // the root before typed deserialization.
    ("max_turns", ConfigKeySchema::Any),
    ("memory_nudge_interval", ConfigKeySchema::Any),
    ("skill_creation_nudge_interval", ConfigKeySchema::Any),
    ("skip_context_files", ConfigKeySchema::Any),
    ("coding_context", ConfigKeySchema::Any),
    ("disabled_toolsets", ConfigKeySchema::Any),
    ("background_review_enabled", ConfigKeySchema::Any),
    ("code_index_enabled", ConfigKeySchema::Any),
    ("code_index_max_files", ConfigKeySchema::Any),
    ("code_index_max_symbols", ConfigKeySchema::Any),
    ("lsp_context_enabled", ConfigKeySchema::Any),
    ("lsp_context_max_chars", ConfigKeySchema::Any),
    ("preflight_context_compress", ConfigKeySchema::Any),
    ("service_tier", ConfigKeySchema::Any),
    ("api_max_retries", ConfigKeySchema::Any),
    ("apiMaxRetries", ConfigKeySchema::Any),
    ("prefill_messages_file", ConfigKeySchema::Any),
];
static DELEGATION_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("max_spawn_depth", ConfigKeySchema::Any),
    ("model", ConfigKeySchema::Any),
    ("provider", ConfigKeySchema::Any),
    ("base_url", ConfigKeySchema::Any),
    ("api_key", ConfigKeySchema::Any),
];
static SESSION_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("reset_policy", ConfigKeySchema::Any),
    ("max_context_messages", ConfigKeySchema::Any),
    ("compression_enabled", ConfigKeySchema::Any),
    ("platform_overrides", ConfigKeySchema::Any),
    ("session_type_overrides", ConfigKeySchema::Any),
];
static SESSIONS_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("auto_prune", ConfigKeySchema::Any),
    ("retention_days", ConfigKeySchema::Any),
    ("vacuum_after_prune", ConfigKeySchema::Any),
    ("min_interval_hours", ConfigKeySchema::Any),
];
static STREAMING_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("enabled", ConfigKeySchema::Any),
    ("edit_interval_ms", ConfigKeySchema::Any),
    ("buffer_threshold", ConfigKeySchema::Any),
    ("max_message_length", ConfigKeySchema::Any),
];
static TOOL_OUTPUT_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("max_bytes", ConfigKeySchema::Any),
    ("max_lines", ConfigKeySchema::Any),
    ("max_line_length", ConfigKeySchema::Any),
];
static BUDGET_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("max_result_size_chars", ConfigKeySchema::Any),
    ("max_aggregate_chars", ConfigKeySchema::Any),
];
static PROXY_CONFIG_KEYS: &[(&str, ConfigKeySchema)] = &[
    ("http_proxy", ConfigKeySchema::Any),
    ("socks_proxy", ConfigKeySchema::Any),
];

const ROOT_CONFIG_COMPATIBILITY_KEYS: &[&str] = &[
    "custom_providers",
    "providers",
    "fallback_providers",
    "toolsets",
    "session_reset",
    "telegram",
    "discord",
    "slack",
    "whatsapp",
    "signal",
    "weixin",
    "matrix",
    "mattermost",
    "dingtalk",
    "feishu",
    "wecom",
    "email",
    "sms",
    "homeassistant",
    "bluebubbles",
];

fn config_schema_unknown_key_error(
    document_label: &str,
    path: &Path,
    key_path: &str,
) -> ConfigError {
    ConfigError::ValidationError(format!(
        "unknown {document_label} key '{key_path}' in {}",
        path.display()
    ))
}

fn mapping_key_as_str<'a>(
    key: &'a serde_yaml::Value,
    document_label: &str,
    path: &Path,
    parent_path: &str,
) -> Result<&'a str, ConfigError> {
    key.as_str().ok_or_else(|| {
        ConfigError::ValidationError(format!(
            "invalid non-string {document_label} key under '{}' in {}",
            parent_path,
            path.display()
        ))
    })
}

fn validate_config_schema(
    value: &serde_yaml::Value,
    schema: &ConfigKeySchema,
    document_label: &str,
    path: &Path,
    parent_path: &str,
) -> Result<(), ConfigError> {
    match schema {
        ConfigKeySchema::Any => Ok(()),
        ConfigKeySchema::Object(keys) | ConfigKeySchema::ObjectWithExtras(keys) => {
            let Some(mapping) = value.as_mapping() else {
                return Ok(());
            };
            let allows_extras = matches!(schema, ConfigKeySchema::ObjectWithExtras(_));
            for (key, value) in mapping {
                let key = mapping_key_as_str(key, document_label, path, parent_path)?;
                let key_path = if parent_path.is_empty() {
                    key.to_string()
                } else {
                    format!("{parent_path}.{key}")
                };
                let Some((_, child_schema)) = keys.iter().find(|(known, _)| *known == key) else {
                    if allows_extras {
                        continue;
                    }
                    return Err(config_schema_unknown_key_error(
                        document_label,
                        path,
                        &key_path,
                    ));
                };
                validate_config_schema(
                    value,
                    child_schema,
                    document_label,
                    path,
                    &key_path,
                )?;
            }
            Ok(())
        }
        ConfigKeySchema::Map(value_schema) => {
            let Some(mapping) = value.as_mapping() else {
                return Ok(());
            };
            for (key, value) in mapping {
                let key = mapping_key_as_str(key, document_label, path, parent_path)?;
                let key_path = if parent_path.is_empty() {
                    key.to_string()
                } else {
                    format!("{parent_path}.{key}")
                };
                validate_config_schema(
                    value,
                    value_schema,
                    document_label,
                    path,
                    &key_path,
                )?;
            }
            Ok(())
        }
        ConfigKeySchema::Sequence(item_schema) => {
            let Some(items) = value.as_sequence() else {
                return Ok(());
            };
            for (index, item) in items.iter().enumerate() {
                let item_path = format!("{parent_path}[{index}]");
                validate_config_schema(
                    item,
                    item_schema,
                    document_label,
                    path,
                    &item_path,
                )?;
            }
            Ok(())
        }
    }
}

fn validate_pre_normalization_root_keys(
    root: &serde_yaml::Value,
    document_label: &str,
    path: &Path,
) -> Result<(), ConfigError> {
    let Some(mapping) = root.as_mapping() else {
        return Ok(());
    };
    for (key, _) in mapping {
        let key = mapping_key_as_str(key, document_label, path, "")?;
        let known = ROOT_CONFIG_KEYS.iter().any(|(known, _)| *known == key)
            || ROOT_CONFIG_COMPATIBILITY_KEYS.contains(&key);
        if !known {
            return Err(config_schema_unknown_key_error(document_label, path, key));
        }
    }
    Ok(())
}

fn validate_config_document_keys(
    root: &serde_yaml::Value,
    strictness: ConfigDocumentStrictness,
    document_label: &str,
    path: &Path,
) -> Result<(), ConfigError> {
    if !strictness.rejects_unknown_keys() {
        return Ok(());
    }
    // Validate before normalization as well: normalization drops ordinary
    // top-level nulls, which must not turn an unknown key into a silent no-op.
    validate_pre_normalization_root_keys(root, document_label, path)
}

fn validate_normalized_config_document_keys(
    root: &serde_yaml::Value,
    strictness: ConfigDocumentStrictness,
    document_label: &str,
    path: &Path,
) -> Result<(), ConfigError> {
    if !strictness.rejects_unknown_keys() {
        return Ok(());
    }
    validate_config_schema(root, &ROOT_CONFIG_KEY_SCHEMA, document_label, path, "")
}

// ---------------------------------------------------------------------------
// ConfigError conversion helpers
// ---------------------------------------------------------------------------

/// Helper function to convert serde_yaml::Error to ConfigError (avoids orphan rule).
fn yaml_to_config_error(e: serde_yaml::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

/// Helper function to convert serde_json::Error to ConfigError (avoids orphan rule).
fn json_to_config_error(e: serde_json::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

/// Helper function to convert std::io::Error to ConfigError (avoids orphan rule).
fn io_to_config_error(e: std::io::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn target_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn atomic_temp_path(path: &Path) -> Result<PathBuf, ConfigError> {
    let parent = target_parent(path);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ConfigError::ParseError(format!("invalid config path: {}", path.display()))
        })?;
    let nonce = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{file_name}.tmp.{}.{}", std::process::id(), nonce)))
}

/// Atomically write bytes to a path by writing a sibling temp file then replacing the destination.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let parent = target_parent(path);
    std::fs::create_dir_all(parent).map_err(io_to_config_error)?;
    let tmp_path = atomic_temp_path(path)?;

    let result = (|| -> Result<(), ConfigError> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&tmp_path).map_err(io_to_config_error)?;
        file.write_all(bytes).map_err(io_to_config_error)?;
        file.sync_all().map_err(io_to_config_error)?;
        drop(file);
        atomic_replace_file(&tmp_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

/// Atomically replace `path` with a fully written sibling temporary file.
///
/// Callers own the temporary file's contents and durability. This operation
/// never removes the destination before replacement, so a failed replacement
/// cannot leave the destination absent.
pub fn atomic_replace_file(tmp_path: &Path, path: &Path) -> Result<(), ConfigError> {
    #[cfg(windows)]
    {
        return atomic_replace_file_windows(tmp_path, path);
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp_path, path).map_err(io_to_config_error)
    }
}

#[cfg(windows)]
fn atomic_replace_file_windows(tmp_path: &Path, path: &Path) -> Result<(), ConfigError> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const REPLACEFILE_WRITE_THROUGH: u32 = 0x0000_0001;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut c_void,
            reserved: *mut c_void,
        ) -> i32;
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let tmp_wide = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replaced = if path.exists() {
        // ReplaceFileW preserves atomic replace semantics for an existing file.
        unsafe {
            ReplaceFileW(
                path_wide.as_ptr(),
                tmp_wide.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }
    } else {
        // The initial create has no file to replace. MoveFileExW still handles
        // a concurrent creator safely because REPLACE_EXISTING is retained.
        unsafe {
            MoveFileExW(
                tmp_wide.as_ptr(),
                path_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if replaced == 0 {
        return Err(io_to_config_error(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(unix)]
fn config_chmod_enabled() -> bool {
    let env_opt_out = ["HERMES_SKIP_CHMOD", "HERMES_CONTAINER"]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()));
    !env_opt_out && !Path::new("/.dockerenv").exists()
}

#[cfg(unix)]
fn secure_config_file(path: &Path) -> Result<(), ConfigError> {
    if !config_chmod_enabled() {
        return Ok(());
    }
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(io_to_config_error)?
        .permissions();
    if permissions.mode() & 0o777 != 0o600 {
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).map_err(io_to_config_error)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_config_file(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

/// Crash-safe JSON write compatible with upstream `utils.atomic_json_write`.
pub fn atomic_json_write(path: &Path, value: &serde_json::Value) -> Result<(), ConfigError> {
    let bytes = serde_json::to_vec(value).map_err(json_to_config_error)?;
    atomic_write_bytes(path, &bytes)
}

/// Crash-safe pretty JSON write for user-facing state files.
pub fn atomic_json_write_pretty(path: &Path, value: &serde_json::Value) -> Result<(), ConfigError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(json_to_config_error)?;
    atomic_write_bytes(path, &bytes)
}

/// Crash-safe YAML write with optional trailing content, matching Python setup helpers.
pub fn atomic_yaml_write(
    path: &Path,
    value: &serde_yaml::Value,
    extra_content: Option<&str>,
) -> Result<(), ConfigError> {
    let mut yaml = serde_yaml::to_string(value)
        .map(|yaml| normalize_yaml_sequence_indent(&yaml))
        .map_err(yaml_to_config_error)?;
    if let Some(extra) = extra_content {
        yaml.push_str(extra);
    }
    atomic_write_bytes(path, yaml.as_bytes())
}

fn normalize_yaml_sequence_indent(yaml: &str) -> String {
    let mut out = String::with_capacity(yaml.len());
    let mut previous_mapping_indent: Option<usize> = None;
    let mut active_sequence_shift: Option<(usize, usize, usize)> = None; // parent, original, shift
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        let indent = line.len().saturating_sub(trimmed.len());
        let mut emitted = false;
        if let Some((parent_indent, original_indent, shift)) = active_sequence_shift {
            if trimmed.starts_with("- ") && indent == original_indent {
                out.push_str(&" ".repeat(shift));
                out.push_str(line);
                emitted = true;
            } else if !trimmed.is_empty() && indent <= parent_indent {
                active_sequence_shift = None;
            } else if indent > original_indent {
                out.push_str(&" ".repeat(shift));
                out.push_str(line);
                emitted = true;
            }
        }
        if trimmed.starts_with("- ") {
            if let Some(parent_indent) = previous_mapping_indent {
                if indent <= parent_indent {
                    let target_indent = parent_indent + 2;
                    let shift = target_indent.saturating_sub(indent);
                    out.push_str(&" ".repeat(target_indent));
                    out.push_str(trimmed);
                    active_sequence_shift = Some((parent_indent, indent, shift));
                    emitted = true;
                }
            }
        }
        if !emitted {
            out.push_str(line);
        }
        out.push('\n');
        if !trimmed.is_empty() && trimmed.ends_with(':') && !trimmed.starts_with("- ") {
            previous_mapping_indent = Some(indent);
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            previous_mapping_indent = None;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// load_dotenv
// ---------------------------------------------------------------------------

/// Load user and project dotenv files into the process environment.
///
/// Behavior mirrors Python parity:
/// - `$HERMES_HOME/.env` overrides stale shell-exported values.
/// - `./.env` is sanitized and loaded as a fallback source.
/// - When user env exists, project env only fills missing keys.
pub fn load_dotenv() {
    let env_file = paths::env_path();
    let project_env = std::env::current_dir().ok().map(|p| p.join(".env"));

    if env_file.exists() {
        sanitize_env_file_if_needed(&env_file);
        // User env should override stale shell exports.
        load_dotenv_file(&env_file, true);
    }

    if let Some(project_env) = project_env.filter(|p| p.exists()) {
        sanitize_env_file_if_needed(&project_env);
        // Project env only fills gaps when user env exists; otherwise it can override.
        load_dotenv_file(&project_env, !env_file.exists());
    }

    if let Some(managed_env) = managed_scope_dir()
        .map(|dir| dir.join(".env"))
        .filter(|path| path.exists())
    {
        // Administrator-pinned env always wins over shell/user/project dotenv.
        load_dotenv_file(&managed_env, true);
    }
}

fn load_dotenv_file(path: &Path, override_existing: bool) {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            let value = strip_quotes(value.trim());
            if override_existing || std::env::var(key).is_err() {
                // SAFETY: called during startup/config loading.
                unsafe { std::env::set_var(key, value) };
            }
        }
    }
}

fn sanitize_env_file_if_needed(path: &Path) {
    let Ok(original) = std::fs::read_to_string(path) else {
        return;
    };
    let sanitized = sanitize_env_lines(&original);
    if sanitized != original {
        let _ = atomic_write_bytes(path, sanitized.as_bytes());
    }
}

fn sanitize_env_lines(contents: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in contents.lines() {
        out.extend(split_concatenated_assignments(line));
    }
    let mut sanitized = out.join("\n");
    if contents.ends_with('\n') {
        sanitized.push('\n');
    }
    sanitized
}

fn split_concatenated_assignments(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return vec![line.to_string()];
    }

    let mut segments = Vec::new();
    let mut current = trimmed.to_string();
    loop {
        let Some((key, value)) = current.split_once('=') else {
            segments.push(current);
            break;
        };
        let key = key.trim();
        if key.is_empty() {
            segments.push(current);
            break;
        }
        if let Some(split_idx) = find_embedded_assignment_start(value) {
            let left = value[..split_idx].trim_end();
            segments.push(format!("{key}={left}"));
            current = value[split_idx..].trim_start().to_string();
            continue;
        }
        segments.push(format!("{key}={}", value.trim()));
        break;
    }
    segments
}

fn find_embedded_assignment_start(value: &str) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_uppercase() {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                let key = &value[i..j];
                if key.len() >= 4 && key.contains('_') {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Return the active managed-scope directory, if configured.
///
/// Python Hermes treats `$HERMES_MANAGED_DIR` as an optional administrator
/// overlay and falls back to `/etc/hermes` in production. Unit tests skip the
/// default path so a developer machine's real managed install cannot leak into
/// deterministic tests.
pub fn managed_scope_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(MANAGED_SCOPE_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty() && path.is_dir())
    {
        return Some(path);
    }

    #[cfg(not(test))]
    {
        let path = PathBuf::from(DEFAULT_MANAGED_SCOPE_DIR);
        if path.is_dir() {
            return Some(path);
        }
    }

    None
}

#[derive(Debug)]
struct ManagedConfigOverlay {
    value: serde_json::Value,
    path: PathBuf,
}

fn load_managed_config_yaml_value() -> Result<Option<(serde_yaml::Value, PathBuf)>, ConfigError> {
    let Some(directory) = managed_scope_dir() else {
        return Ok(None);
    };
    let path = directory.join("config.yaml");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigError::ParseError(format!(
                "failed to read managed config overlay {}: {err}",
                path.display()
            )));
        }
    };
    if contents.trim().is_empty() {
        return Ok(None);
    }

    let mut root: serde_yaml::Value = serde_yaml::from_str(&contents).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to parse managed config overlay {}: {err}",
            path.display()
        ))
    })?;
    validate_config_document_keys(
        &root,
        ConfigDocumentStrictness::Strict,
        "managed config",
        &path,
    )?;
    expand_env_vars_in_yaml(&mut root);
    let serde_yaml::Value::Mapping(map) = &mut root else {
        return Err(ConfigError::ValidationError(format!(
            "managed config overlay {} must be a YAML mapping",
            path.display()
        )));
    };
    crate::python_yaml_compat::normalize_config_yaml_root(map);
    mark_platform_enabled_explicit(&mut root, "slack");
    mark_platform_enabled_explicit(&mut root, "ntfy");
    validate_normalized_config_document_keys(
        &root,
        ConfigDocumentStrictness::Strict,
        "managed config",
        &path,
    )?;

    let config: GatewayConfig = serde_yaml::from_value(root.clone()).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to deserialize managed config overlay {}: {err}",
            path.display()
        ))
    })?;
    validate_execution_mode_config_value(
        &config.execution_mode,
        ExecutionModeSource::ConfigFile,
        Some(&path),
        "managed config overlay",
    )?;
    validate_config(&config).map_err(|err| {
        ConfigError::ValidationError(format!(
            "invalid managed config overlay {}: {err}",
            path.display()
        ))
    })?;

    Ok(Some((root, path)))
}

fn load_managed_config_overlay_json() -> Result<Option<ManagedConfigOverlay>, ConfigError> {
    let Some((yaml, path)) = load_managed_config_yaml_value()? else {
        return Ok(None);
    };
    let value = serde_json::to_value(yaml).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to convert managed config overlay {}: {err}",
            path.display()
        ))
    })?;
    if !value.is_object() {
        return Err(ConfigError::ValidationError(format!(
            "managed config overlay {} must be a mapping",
            path.display()
        )));
    }
    Ok(Some(ManagedConfigOverlay { value, path }))
}

fn read_config_yaml_as_json(path: &Path) -> Result<Option<serde_json::Value>, ConfigError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigError::ParseError(format!(
                "failed to read config file {}: {err}",
                path.display()
            )))
        }
    };
    if contents.trim().is_empty() {
        return Ok(Some(serde_json::json!({})));
    }
    let value = serde_yaml::from_str::<serde_json::Value>(&contents).map_err(|err| {
        ConfigError::ParseError(format!("failed to parse config file {}: {err}", path.display()))
    })?;
    if value.is_object() {
        Ok(Some(value))
    } else {
        Err(ConfigError::ValidationError(format!(
            "config file {} must be a mapping",
            path.display()
        )))
    }
}

/// Load raw user config plus managed-scope overlay for standalone runtime readers.
///
/// This intentionally returns a raw value instead of [`GatewayConfig`] so
/// narrow subsystems can keep reading upstream-compatible keys that the typed
/// config model does not own, while still honoring administrator-pinned leaves.
pub fn load_effective_config_yaml_value(path: &Path) -> Option<serde_json::Value> {
    let ignore_user_config = env_truthy("HERMES_IGNORE_USER_CONFIG");
    let mut base = if ignore_user_config {
        serde_json::json!({})
    } else {
        match read_config_yaml_as_json(path) {
            Ok(Some(value)) => {
                cache_last_known_good_raw_config(path, &value);
                value
            }
            Ok(None) => serde_json::json!({}),
            Err(error) if retains_last_known_good_config(&error) => {
                let cached = last_known_good_raw_config(path)?;
                tracing::error!(
                    "config file {} was rejected; retaining the last successfully validated raw configuration: {error}",
                    path.display()
                );
                cached
            }
            Err(error) => {
                tracing::error!("config file {} was rejected: {error}", path.display());
                return None;
            }
        }
    };
    let had_user = base.as_object().is_some_and(|obj| !obj.is_empty());

    let managed = match load_managed_config_overlay_json() {
        Ok(overlay) => overlay,
        Err(err) => {
            tracing::error!("managed scope configuration rejected: {err}");
            return None;
        }
    };
    if let Some(overlay) = &managed {
        deep_merge(&mut base, &overlay.value);
    }

    if had_user || managed.is_some() {
        Some(base)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// load_config
// ---------------------------------------------------------------------------

/// Load the full configuration, applying the priority chain:
///
///   managed config/env > env vars > .env > cli-config.yaml > config.yaml > gateway.json > defaults
///
/// If `home_dir` is provided it overrides the `HERMES_HOME` env var.
pub fn load_config(home_dir: Option<&str>) -> Result<GatewayConfig, ConfigError> {
    // Load .env before anything else so env overrides see those values.
    load_dotenv();

    // Determine effective hermes home
    let effective_home = home_dir
        .map(|s| s.to_string())
        .or_else(|| std::env::var("HERMES_HOME").ok())
        .unwrap_or_else(|| paths::hermes_home().to_string_lossy().to_string());

    let config_yaml_path = Path::new(&effective_home).join("config.yaml");
    let cli_config_yaml_path = Path::new(&effective_home).join("cli-config.yaml");
    let gateway_json_path = Path::new(&effective_home).join("gateway.json");
    let ignore_user_config = env_truthy("HERMES_IGNORE_USER_CONFIG");

    // Start from defaults
    let mut config = GatewayConfig::default();

    // Layer 1: gateway.json (lowest priority file source). A present file is
    // authoritative input, so malformed or invalid values cannot be silently
    // bypassed by a later layer.
    if !ignore_user_config && gateway_json_path.exists() {
        config = load_from_json(&gateway_json_path)?;
    }

    // Layer 2: config.yaml (higher priority file source). A malformed or
    // invalid replacement only retains a snapshot that this process already
    // parsed and validated; an initial load still fails closed.
    if !ignore_user_config && config_yaml_path.exists() {
        let yaml_cfg = match load_from_yaml(&config_yaml_path) {
            Ok(config) => {
                cache_last_known_good_typed_config(&config_yaml_path, &config);
                config
            }
            Err(error) if retains_last_known_good_config(&error) => {
                let Some(cached) = last_known_good_typed_config(&config_yaml_path) else {
                    return Err(error);
                };
                tracing::error!(
                    "config file {} was rejected; retaining the last successfully validated configuration: {error}",
                    config_yaml_path.display()
                );
                cached
            }
            Err(error) => return Err(error),
        };
        config = merge_configs(&yaml_cfg, &config);
    }

    // Layer 2.5: cli-config.yaml (CLI-specific + high-priority overlay).
    if !ignore_user_config && cli_config_yaml_path.exists() {
        let cli_cfg = load_from_yaml(&cli_config_yaml_path)?;
        config = merge_configs(&cli_cfg, &config);
    }

    bridge_terminal_config_to_env(&config.terminal);
    bridge_web_config_to_env(&config.web);
    bridge_display_config_to_env(&config.display);

    // Layer 3: environment variables (highest priority)
    apply_env_overrides(&mut config);
    normalize_platform_aliases(&mut config);
    normalize_provider_secrets(&mut config);

    let managed_overlay = load_managed_config_overlay_json()?;
    if let Some(overlay) = &managed_overlay {
        let mut managed_config = config.clone();
        apply_managed_config_overlay(&mut managed_config, &overlay.value).map_err(|err| {
            ConfigError::ValidationError(format!(
                "invalid managed config overlay {}: {err}",
                overlay.path.display()
            ))
        })?;
        normalize_platform_aliases(&mut managed_config);
        normalize_provider_secrets(&mut managed_config);
        validate_config(&managed_config).map_err(|err| {
            ConfigError::ValidationError(format!(
                "invalid managed config overlay {}: {err}",
                overlay.path.display()
            ))
        })?;
        config = managed_config;
        bridge_managed_overlay_to_env(&config, &overlay.value);
    }

    // Record the effective home dir
    config.home_dir = Some(effective_home);

    // Validate
    validate_config(&config)?;

    Ok(config)
}

/// Whether a resolution layer was omitted, explicitly null, configured, or
/// unavailable because the layer itself could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionModeLayerPresence {
    Absent,
    ExplicitNull,
    Value(String),
    Unavailable,
}

/// Provenance for a layer-level execution-mode validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionModeLayerError {
    pub source: ExecutionModeSource,
    pub message: String,
}

/// Typed diagnostic state for one execution-mode precedence layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionModeLayerDiagnostic {
    pub source: ExecutionModeSource,
    pub presence: ExecutionModeLayerPresence,
    pub path: Option<PathBuf>,
    pub error: Option<ExecutionModeLayerError>,
}

/// Resolution diagnostic for the startup execution posture.
///
/// `mode` and `source` are set only when the shared core resolver accepted
/// every present layer. `layers` always records every layer's presence and
/// layer-local failure provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfigDiagnostic {
    pub mode: Option<ExecutionMode>,
    pub source: Option<ExecutionModeSource>,
    pub layers: [ExecutionModeLayerDiagnostic; 5],
    pub error: Option<ExecutionModeLayerError>,
}

fn expected_execution_modes() -> String {
    ExecutionMode::ALL
        .iter()
        .map(|mode| mode.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn execution_mode_validation_error(
    source: ExecutionModeSource,
    path: Option<&Path>,
    reason: impl std::fmt::Display,
) -> ConfigError {
    let location = path
        .map(|path| format!(" in {}", path.display()))
        .unwrap_or_default();
    ConfigError::ValidationError(format!(
        "invalid {source} execution mode{location}: {reason}"
    ))
}

fn validate_raw_execution_mode(
    value: &str,
    source: ExecutionModeSource,
    path: Option<&Path>,
) -> Result<(), ConfigError> {
    let result = match source {
        ExecutionModeSource::BuiltInDefault => return Ok(()),
        ExecutionModeSource::Profile => {
            resolve_execution_mode(ExecutionMode::Manual, Some(value), None, None, None)
        }
        ExecutionModeSource::ConfigFile => {
            resolve_execution_mode(ExecutionMode::Manual, None, Some(value), None, None)
        }
        ExecutionModeSource::Environment => {
            resolve_execution_mode(ExecutionMode::Manual, None, None, Some(value), None)
        }
        ExecutionModeSource::Cli => {
            resolve_execution_mode(ExecutionMode::Manual, None, None, None, Some(value))
        }
    };
    result
        .map(|_| ())
        .map_err(|error| execution_mode_validation_error(source, path, error.reason))
}

fn validate_execution_mode_config_value(
    value: &ExecutionModeConfigValue,
    source: ExecutionModeSource,
    path: Option<&Path>,
    _document_label: &str,
) -> Result<(), ConfigError> {
    match value {
        ExecutionModeConfigValue::Absent => Ok(()),
        ExecutionModeConfigValue::ExplicitNull => Err(execution_mode_validation_error(
            source,
            path,
            format!("null (expected {})", expected_execution_modes()),
        )),
        ExecutionModeConfigValue::Value(value) => validate_raw_execution_mode(value, source, path),
    }
}

fn execution_mode_presence(value: &ExecutionModeConfigValue) -> ExecutionModeLayerPresence {
    match value {
        ExecutionModeConfigValue::Absent => ExecutionModeLayerPresence::Absent,
        ExecutionModeConfigValue::ExplicitNull => ExecutionModeLayerPresence::ExplicitNull,
        ExecutionModeConfigValue::Value(value) => ExecutionModeLayerPresence::Value(value.clone()),
    }
}

fn diagnostic_layer_from_presence(
    source: ExecutionModeSource,
    presence: ExecutionModeLayerPresence,
    path: Option<PathBuf>,
) -> ExecutionModeLayerDiagnostic {
    let error = match &presence {
        ExecutionModeLayerPresence::Absent | ExecutionModeLayerPresence::Unavailable => None,
        ExecutionModeLayerPresence::ExplicitNull => validate_execution_mode_config_value(
            &ExecutionModeConfigValue::ExplicitNull,
            source,
            path.as_deref(),
            "diagnostic",
        )
        .err()
        .map(|error| ExecutionModeLayerError {
            source,
            message: error.to_string(),
        }),
        ExecutionModeLayerPresence::Value(value) => validate_raw_execution_mode(
            value,
            source,
            path.as_deref(),
        )
        .err()
        .map(|error| ExecutionModeLayerError {
            source,
            message: error.to_string(),
        }),
    };
    ExecutionModeLayerDiagnostic {
        source,
        presence,
        path,
        error,
    }
}

fn unavailable_diagnostic_layer(
    source: ExecutionModeSource,
    path: Option<PathBuf>,
    error: ConfigError,
) -> ExecutionModeLayerDiagnostic {
    ExecutionModeLayerDiagnostic {
        source,
        presence: ExecutionModeLayerPresence::Unavailable,
        path,
        error: Some(ExecutionModeLayerError {
            source,
            message: error.to_string(),
        }),
    }
}

fn raw_execution_mode_from_presence(presence: &ExecutionModeLayerPresence) -> Option<&str> {
    match presence {
        ExecutionModeLayerPresence::Value(value) => Some(value),
        ExecutionModeLayerPresence::Absent
        | ExecutionModeLayerPresence::ExplicitNull
        | ExecutionModeLayerPresence::Unavailable => None,
    }
}

/// Return a complete, typed diagnostic without duplicating resolution
/// precedence. The final winner is always selected by
/// [`hermes_core::resolve_execution_mode`].
pub fn diagnose_runtime_execution_mode(
    config: &GatewayConfig,
    cli: Option<&str>,
) -> ResolvedConfigDiagnostic {
    let built_in = diagnostic_layer_from_presence(
        ExecutionModeSource::BuiltInDefault,
        ExecutionModeLayerPresence::Value(ExecutionMode::Manual.as_str().to_string()),
        None,
    );

    let profile = match load_selected_profile_execution_mode_with_path(config) {
        Ok(selected) => diagnostic_layer_from_presence(
            ExecutionModeSource::Profile,
            execution_mode_presence(&selected.value),
            selected.path,
        ),
        Err(error) => unavailable_diagnostic_layer(ExecutionModeSource::Profile, None, error),
    };
    let config_file = diagnostic_layer_from_presence(
        ExecutionModeSource::ConfigFile,
        execution_mode_presence(&config.execution_mode),
        None,
    );
    let environment = match execution_mode_from_environment() {
        Ok(Some(value)) => diagnostic_layer_from_presence(
            ExecutionModeSource::Environment,
            ExecutionModeLayerPresence::Value(value),
            None,
        ),
        Ok(None) => diagnostic_layer_from_presence(
            ExecutionModeSource::Environment,
            ExecutionModeLayerPresence::Absent,
            None,
        ),
        Err(error) => unavailable_diagnostic_layer(ExecutionModeSource::Environment, None, error),
    };
    let cli = diagnostic_layer_from_presence(
        ExecutionModeSource::Cli,
        cli.map(|value| ExecutionModeLayerPresence::Value(value.to_string()))
            .unwrap_or(ExecutionModeLayerPresence::Absent),
        None,
    );
    let mut layers = [built_in, profile, config_file, environment, cli];
    let error = layers
        .iter()
        .skip(1)
        .find_map(|layer| layer.error.clone());
    if let Some(error) = error {
        return ResolvedConfigDiagnostic {
            mode: None,
            source: None,
            layers,
            error: Some(error),
        };
    }

    let result = resolve_execution_mode(
        ExecutionMode::Manual,
        raw_execution_mode_from_presence(&layers[1].presence),
        raw_execution_mode_from_presence(&layers[2].presence),
        raw_execution_mode_from_presence(&layers[3].presence),
        raw_execution_mode_from_presence(&layers[4].presence),
    );
    match result {
        Ok(resolved) => ResolvedConfigDiagnostic {
            mode: Some(resolved.mode),
            source: Some(resolved.source),
            layers,
            error: None,
        },
        Err(error) => {
            let layer_error = ExecutionModeLayerError {
                source: error.layer,
                message: error.to_string(),
            };
            if let Some(layer) = layers
                .iter_mut()
                .find(|layer| layer.source == error.layer)
            {
                layer.error = Some(layer_error.clone());
            }
            ResolvedConfigDiagnostic {
                mode: None,
                source: None,
                layers,
                error: Some(layer_error),
            }
        }
    }
}

fn execution_mode_value_at_layer<'a>(
    value: &'a ExecutionModeConfigValue,
    source: ExecutionModeSource,
    path: Option<&Path>,
) -> Result<Option<&'a str>, ConfigError> {
    validate_execution_mode_config_value(value, source, path, "runtime resolution")?;
    match value {
        ExecutionModeConfigValue::Absent | ExecutionModeConfigValue::ExplicitNull => Ok(None),
        ExecutionModeConfigValue::Value(value) => Ok(Some(value)),
    }
}

/// Resolve the execution posture from the complete startup precedence chain.
///
/// Every present source is passed to the shared core resolver. Invalid lower
/// layers therefore fail closed even when a higher-priority layer is valid.
pub fn resolve_runtime_execution_mode(
    config: &GatewayConfig,
    cli: Option<&str>,
) -> Result<ResolvedExecutionMode, ConfigError> {
    let selected_profile = load_selected_profile_execution_mode_with_path(config)?;
    let profile = execution_mode_value_at_layer(
        &selected_profile.value,
        ExecutionModeSource::Profile,
        selected_profile.path.as_deref(),
    )?;
    let config_file = execution_mode_value_at_layer(
        &config.execution_mode,
        ExecutionModeSource::ConfigFile,
        None,
    )?;
    let environment = execution_mode_from_environment()?;
    let resolved = resolve_execution_mode(
        ExecutionMode::Manual,
        profile,
        config_file,
        environment.as_deref(),
        cli,
    )
    .map_err(|error| ConfigError::ValidationError(error.to_string()))?;

    tracing::debug!(
        execution_mode = %resolved.mode,
        execution_mode_source = %resolved.source,
        "Resolved execution mode"
    );
    Ok(resolved)
}

/// Return the raw execution-mode value from the selected profile, preserving
/// absent, explicit-null, and string values for source-specific resolution.
pub fn load_selected_profile_execution_mode(
    config: &GatewayConfig,
) -> Result<ExecutionModeConfigValue, ConfigError> {
    load_selected_profile_execution_mode_with_path(config).map(|selected| selected.value)
}

struct SelectedProfileExecutionMode {
    value: ExecutionModeConfigValue,
    path: Option<PathBuf>,
}

fn load_selected_profile_execution_mode_with_path(
    config: &GatewayConfig,
) -> Result<SelectedProfileExecutionMode, ConfigError> {
    let home = config
        .home_dir
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(paths::hermes_home);
    let profiles_dir = home.join("profiles");
    let profile_name = match config.profile.current.as_deref() {
        Some(name) => Some(validate_execution_mode_profile_name(name)?),
        None => load_active_profile_name_for_execution_mode(&profiles_dir)?,
    };
    let Some(profile_name) = profile_name else {
        return Ok(SelectedProfileExecutionMode {
            value: ExecutionModeConfigValue::Absent,
            path: None,
        });
    };

    let yaml_path = profiles_dir.join(format!("{profile_name}.yaml"));
    let yml_path = profiles_dir.join(format!("{profile_name}.yml"));
    let profile_path = if yaml_path.is_file() {
        yaml_path
    } else if yml_path.is_file() {
        yml_path
    } else {
        return Err(ConfigError::NotFound(format!(
            "selected profile '{profile_name}' does not exist under {}",
            profiles_dir.display()
        )));
    };
    let raw = std::fs::read_to_string(&profile_path).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to read profile execution mode {}: {err}",
            profile_path.display()
        ))
    })?;
    let value = serde_yaml::from_str::<serde_yaml::Value>(&raw).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to parse profile execution mode {}: {err}",
            profile_path.display()
        ))
    })?;
    let Some(mapping) = value.as_mapping() else {
        return Ok(SelectedProfileExecutionMode {
            value: ExecutionModeConfigValue::Absent,
            path: Some(profile_path),
        });
    };
    let key = serde_yaml::Value::String("execution_mode".to_string());
    let value = match mapping.get(&key) {
        None => ExecutionModeConfigValue::Absent,
        Some(serde_yaml::Value::Null) => ExecutionModeConfigValue::ExplicitNull,
        Some(serde_yaml::Value::String(value)) => ExecutionModeConfigValue::Value(value.clone()),
        Some(_) => {
            return Err(ConfigError::ValidationError(format!(
                "invalid profile execution mode in {}: expected string or null",
                profile_path.display()
            )));
        }
    };
    Ok(SelectedProfileExecutionMode {
        value,
        path: Some(profile_path),
    })
}

fn execution_mode_from_environment() -> Result<Option<String>, ConfigError> {
    match std::env::var(EXECUTION_MODE_ENV_KEY) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::ValidationError(format!(
            "{EXECUTION_MODE_ENV_KEY} must be valid UTF-8"
        ))),
    }
}

fn load_active_profile_name_for_execution_mode(
    profiles_dir: &Path,
) -> Result<Option<String>, ConfigError> {
    let active_path = profiles_dir.join(".active_profile");
    if !active_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&active_path).map_err(io_to_config_error)?;
    let name = raw.trim();
    if name.is_empty() {
        return Ok(None);
    }
    validate_execution_mode_profile_name(name).map(Some)
}

fn validate_execution_mode_profile_name(raw: &str) -> Result<String, ConfigError> {
    let name = raw.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(ConfigError::ValidationError(
            "selected profile name is invalid".to_string(),
        ));
    }
    Ok(name.to_string())
}

fn validate_managed_overlay_keys(overlay: &serde_json::Value) -> Result<(), ConfigError> {
    let value = serde_yaml::to_value(overlay).map_err(|err| {
        ConfigError::ParseError(format!("failed to inspect managed config overlay keys: {err}"))
    })?;
    validate_config_schema(
        &value,
        &ROOT_CONFIG_KEY_SCHEMA,
        "managed config overlay",
        Path::new("<managed config overlay>"),
        "",
    )
}

fn apply_managed_config_overlay(
    config: &mut GatewayConfig,
    overlay: &serde_json::Value,
) -> Result<(), ConfigError> {
    validate_managed_overlay_keys(overlay)?;
    let original = config.clone();
    let mut base = serde_json::to_value(&*config).map_err(|err| {
        ConfigError::ParseError(format!("failed to serialize base config for managed overlay: {err}"))
    })?;
    let overlay_execution_mode_is_null = overlay
        .get("execution_mode")
        .is_some_and(serde_json::Value::is_null);
    deep_merge(&mut base, overlay);
    let mut merged: GatewayConfig = serde_json::from_value(base).map_err(|err| {
        ConfigError::ValidationError(format!("failed to apply managed config overlay: {err}"))
    })?;
    if overlay_execution_mode_is_null {
        merged.execution_mode = ExecutionModeConfigValue::ExplicitNull;
    }
    validate_config(&merged)?;
    merged.home_dir = original.home_dir;
    *config = merged;
    Ok(())
}

fn bridge_managed_overlay_to_env(config: &GatewayConfig, overlay: &serde_json::Value) {
    if overlay.get("model").is_some() {
        if let Some(model) = config.model.as_deref() {
            set_env_override("HERMES_MODEL", model.to_string());
        }
    }
    if overlay.get("personality").is_some() {
        if let Some(personality) = config.personality.as_deref() {
            set_env_override("HERMES_PERSONALITY", personality.to_string());
        }
    }
    if overlay.get("max_turns").is_some() {
        set_env_override("HERMES_MAX_TURNS", config.max_turns.to_string());
    }
    if overlay.get("system_prompt").is_some() {
        if let Some(prompt) = config.system_prompt.as_deref() {
            set_env_override("HERMES_SYSTEM_PROMPT", prompt.to_string());
        }
    }
    if overlay.get("prefill_messages_file").is_some() {
        if let Some(path) = config.prefill_messages_file.as_deref() {
            set_env_override("HERMES_PREFILL_MESSAGES_FILE", path.to_string());
        }
    }
    if let Some(kanban) = overlay.get("kanban").and_then(|value| value.as_object()) {
        if kanban.contains_key("dispatch_in_gateway") {
            set_env_override(
                "HERMES_KANBAN_DISPATCH_IN_GATEWAY",
                config.kanban.dispatch_in_gateway.to_string(),
            );
        }
    }
    if let Some(security) = overlay.get("security").and_then(|value| value.as_object()) {
        if security.contains_key("allow_private_urls") {
            set_env_override(
                "HERMES_ALLOW_PRIVATE_URLS",
                config.security.allow_private_urls.to_string(),
            );
        }
    }
    bridge_managed_section_to_env(
        &config.terminal,
        overlay.get("terminal"),
        terminal_config_env_bridge_pairs(),
    );
    bridge_managed_section_to_env(
        &config.web,
        overlay.get("web"),
        web_config_env_bridge_pairs(),
    );
    bridge_managed_section_to_env(
        &config.display,
        overlay.get("display"),
        display_config_env_bridge_pairs(),
    );
}

fn bridge_managed_section_to_env<T: serde::Serialize>(
    config: &T,
    overlay: Option<&serde_json::Value>,
    pairs: &[(&str, &str)],
) {
    let Some(overlay_map) = overlay.and_then(|value| value.as_object()) else {
        return;
    };
    let config_value = match serde_json::to_value(config) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("managed scope: failed to bridge config section to env: {err}");
            return;
        }
    };
    for (overlay_key, env_key) in pairs {
        if !overlay_map.contains_key(*overlay_key) {
            continue;
        }
        let config_key = match *overlay_key {
            "env_type" => "backend",
            "cwd" => "workdir",
            key => key,
        };
        if let Some(value) = config_value
            .get(config_key)
            .and_then(json_value_to_env_string)
        {
            set_env_override(env_key, value);
        }
    }
}

fn json_value_to_env_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Array(values) => {
            let joined = values
                .iter()
                .filter_map(json_value_to_env_string)
                .collect::<Vec<_>>()
                .join(",");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn set_env_override(name: &str, value: String) {
    if value.trim().is_empty() {
        return;
    }
    // SAFETY: configuration loading runs during CLI/gateway startup.
    unsafe { std::env::set_var(name, value) };
}

// ---------------------------------------------------------------------------
// load_from_yaml
// ---------------------------------------------------------------------------

/// Load a GatewayConfig from a YAML file in strict mode.
pub fn load_from_yaml(path: &Path) -> Result<GatewayConfig, ConfigError> {
    load_from_yaml_inner(path, true, ConfigDocumentStrictness::Strict)
}


fn load_from_yaml_preserving_env_refs_with_options(
    path: &Path,
    options: UserConfigLoadOptions,
) -> Result<GatewayConfig, ConfigError> {
    load_from_yaml_inner(
        path,
        false,
        ConfigDocumentStrictness::from_user_options(options),
    )
}

fn load_from_yaml_inner(
    path: &Path,
    expand_env_refs: bool,
    strictness: ConfigDocumentStrictness,
) -> Result<GatewayConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|err| {
        ConfigError::ParseError(format!("failed to read config file {}: {err}", path.display()))
    })?;
    let mut root: serde_yaml::Value = serde_yaml::from_str(&contents).map_err(|err| {
        ConfigError::ParseError(format!("failed to parse config file {}: {err}", path.display()))
    })?;
    validate_config_document_keys(&root, strictness, "config", path)?;
    if expand_env_refs {
        expand_env_vars_in_yaml(&mut root);
    }
    if let serde_yaml::Value::Mapping(map) = &mut root {
        crate::python_yaml_compat::normalize_config_yaml_root(map);
    }
    mark_platform_enabled_explicit(&mut root, "slack");
    mark_platform_enabled_explicit(&mut root, "ntfy");
    validate_normalized_config_document_keys(&root, strictness, "config", path)?;
    let mut config: GatewayConfig = serde_yaml::from_value(root).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to deserialize config file {}: {err}",
            path.display()
        ))
    })?;
    validate_execution_mode_config_value(
        &config.execution_mode,
        ExecutionModeSource::ConfigFile,
        Some(path),
        "config file",
    )?;
    normalize_platform_aliases(&mut config);
    validate_config(&config).map_err(|err| {
        ConfigError::ValidationError(format!(
            "invalid config file {}: {err}",
            path.display()
        ))
    })?;
    Ok(config)
}

fn expand_env_vars_in_yaml(value: &mut serde_yaml::Value) {
    match value {
        serde_yaml::Value::String(s) => {
            *s = expand_env_refs_in_str(s);
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                expand_env_vars_in_yaml(item);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for value in map.values_mut() {
                expand_env_vars_in_yaml(value);
            }
        }
        _ => {}
    }
}

fn expand_env_refs_in_str(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let raw_reference = &after_open[..end];
        let reference = raw_reference.trim();
        let name = reference
            .strip_prefix("env:")
            .map(str::trim)
            .unwrap_or(reference);
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            match std::env::var(name) {
                Ok(value) => output.push_str(&value),
                Err(_) => {
                    output.push_str("${");
                    output.push_str(raw_reference);
                    output.push('}');
                }
            }
        } else {
            if is_non_env_secret_reference(reference) {
                tracing::warn!(
                    "config reference '${{{reference}}}' is not an environment reference; configure it in the secrets block instead"
                );
            }
            output.push_str("${");
            output.push_str(raw_reference);
            output.push('}');
        }
        rest = &after_open[end + 1..];
    }
    output.push_str(rest);
    output
}

fn is_non_env_secret_reference(reference: &str) -> bool {
    let Some((source, _)) = reference.split_once(':') else {
        return false;
    };
    !reference.starts_with("env:")
        && source
            .bytes()
            .enumerate()
            .all(|(index, byte)| match index {
                0 => byte.is_ascii_lowercase(),
                _ => byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-',
            })
}

fn mark_platform_enabled_explicit(root: &mut serde_yaml::Value, platform: &str) {
    let serde_yaml::Value::Mapping(root_map) = root else {
        return;
    };
    let platforms_key = serde_yaml::Value::String("platforms".to_string());
    let platform_key = serde_yaml::Value::String(platform.to_string());
    let enabled_key = serde_yaml::Value::String("enabled".to_string());
    let extra_key = serde_yaml::Value::String("extra".to_string());
    let marker_key = serde_yaml::Value::String("_enabled_explicit".to_string());

    let Some(serde_yaml::Value::Mapping(platforms)) = root_map.get_mut(&platforms_key) else {
        return;
    };
    let Some(serde_yaml::Value::Mapping(platform_block)) = platforms.get_mut(&platform_key) else {
        return;
    };
    if !platform_block.contains_key(&enabled_key) {
        return;
    }

    let extra_entry = platform_block
        .entry(extra_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if let serde_yaml::Value::Mapping(extra_map) = extra_entry {
        extra_map.insert(marker_key, serde_yaml::Value::Bool(true));
    }
}

fn normalize_platform_aliases(config: &mut GatewayConfig) {
    normalize_discord_allow_from_alias(config);
}

fn normalize_discord_allow_from_alias(config: &mut GatewayConfig) {
    let Some(discord) = config.platforms.get_mut("discord") else {
        return;
    };
    if !discord.allowed_users.is_empty() {
        return;
    }

    let alias_users = {
        let direct = discord.extra.get("allow_from");
        let nested = discord
            .extra
            .get("extra")
            .and_then(|value| value.get("allow_from"));
        direct
            .and_then(json_value_to_string_list)
            .or_else(|| nested.and_then(json_value_to_string_list))
            .filter(|users| !users.is_empty())
    };

    if let Some(users) = alias_users {
        discord.allowed_users = users;
    }
}

fn json_value_to_string_list(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::String(raw) => Some(comma_separated_values(raw)),
        serde_json::Value::Array(items) => {
            let values = items
                .iter()
                .filter_map(json_scalar_to_string)
                .flat_map(|raw| comma_separated_values(&raw))
                .collect::<Vec<_>>();
            Some(values)
        }
        _ => json_scalar_to_string(value).map(|raw| comma_separated_values(&raw)),
    }
}

fn json_scalar_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(raw) => Some(raw.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn comma_separated_values(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Load `config.yaml` from disk in strict mode if it exists; otherwise return
/// defaults (no environment merge).
pub fn load_user_config_file(path: &Path) -> Result<GatewayConfig, ConfigError> {
    load_user_config_file_with_options(path, UserConfigLoadOptions::strict())
}

/// Load a user-owned config document with an explicit compatibility policy.
///
/// Managed overlays never use this escape hatch and remain strict.
pub fn load_user_config_file_with_options(
    path: &Path,
    options: UserConfigLoadOptions,
) -> Result<GatewayConfig, ConfigError> {
    if path.exists() {
        let mut cfg = load_from_yaml_preserving_env_refs_with_options(path, options)?;
        normalize_platform_aliases(&mut cfg);
        normalize_provider_secrets(&mut cfg);
        validate_config(&cfg)?;
        Ok(cfg)
    } else {
        Ok(GatewayConfig::default())
    }
}

fn normalize_provider_secrets(config: &mut GatewayConfig) {
    for provider in config.llm_providers.values_mut() {
        if provider
            .api_key
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.api_key = None;
        }
        if provider
            .api_key_env
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.api_key_env = None;
        }
        if provider
            .base_url
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.base_url = None;
        }
        if provider
            .oauth_token_url
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.oauth_token_url = None;
        }
        if provider
            .oauth_client_id
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.oauth_client_id = None;
        }
    }

    config.llm_providers.retain(|_, provider| {
        provider.api_key.is_some()
            || provider.api_key_env.is_some()
            || provider.base_url.is_some()
            || provider.command.is_some()
            || !provider.args.is_empty()
            || provider.model.is_some()
            || !provider.models.is_empty()
            || !provider.discover_models
            || provider.api_mode.is_some()
            || provider.max_tokens.is_some()
            || provider.temperature.is_some()
            || provider.extra_body.is_some()
            || provider.rate_limit.is_some()
            || !provider.credential_pool.is_empty()
            || provider.request_timeout_seconds.is_some()
            || provider.oauth_token_url.is_some()
            || provider.oauth_client_id.is_some()
    });
}

include!("loader/user_config_patch.rs");

include!("loader/config_write.rs");

// ---------------------------------------------------------------------------
// load_from_json
// ---------------------------------------------------------------------------

/// Load a GatewayConfig from a JSON file in strict mode.
pub fn load_from_json(path: &Path) -> Result<GatewayConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|err| {
        ConfigError::ParseError(format!("failed to read config file {}: {err}", path.display()))
    })?;
    let value: serde_json::Value = serde_json::from_str(&contents).map_err(|err| {
        ConfigError::ParseError(format!("failed to parse config file {}: {err}", path.display()))
    })?;
    let yaml_value = serde_yaml::to_value(&value).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to inspect config file {}: {err}",
            path.display()
        ))
    })?;
    validate_config_schema(&yaml_value, &ROOT_CONFIG_KEY_SCHEMA, "config", path, "")?;
    let mut config: GatewayConfig = serde_json::from_value(value).map_err(|err| {
        ConfigError::ParseError(format!(
            "failed to deserialize config file {}: {err}",
            path.display()
        ))
    })?;
    validate_execution_mode_config_value(
        &config.execution_mode,
        ExecutionModeSource::ConfigFile,
        Some(path),
        "config file",
    )?;
    normalize_platform_aliases(&mut config);
    validate_config(&config).map_err(|err| {
        ConfigError::ValidationError(format!(
            "invalid config file {}: {err}",
            path.display()
        ))
    })?;
    Ok(config)
}

/// Resolve the prefill JSON file using upstream-compatible precedence.
///
/// `prefill_messages_file` at the top level is canonical. The nested
/// `agent.prefill_messages_file` key remains a legacy fallback for older
/// generated configs.
pub fn resolve_prefill_messages_file(config: &GatewayConfig) -> Option<String> {
    env_var_nonempty("HERMES_PREFILL_MESSAGES_FILE")
        .or_else(|| trimmed_optional(config.prefill_messages_file.as_deref()))
        .or_else(|| trimmed_optional(config.agent.prefill_messages_file.as_deref()))
}

/// Resolve a configured prefill JSON path, mapping relative paths under HERMES_HOME.
pub fn resolve_prefill_messages_path(config: &GatewayConfig) -> Option<PathBuf> {
    let path = expand_home_path(&resolve_prefill_messages_file(config)?);
    if path.is_absolute() {
        return Some(path);
    }
    let home = config
        .home_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(paths::hermes_home);
    Some(home.join(path))
}

/// Load configured ephemeral prefill messages.
///
/// Invalid or missing files match Python behavior: warn and continue without
/// prefill rather than blocking startup.
pub fn load_prefill_messages(config: &GatewayConfig) -> Vec<hermes_core::Message> {
    let Some(path) = resolve_prefill_messages_path(config) else {
        return Vec::new();
    };
    load_prefill_messages_file(&path)
}

pub fn load_prefill_messages_file(path: &Path) -> Vec<hermes_core::Message> {
    if !path.exists() {
        tracing::warn!("Prefill messages file not found: {}", path.display());
        return Vec::new();
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!(
                "Failed to load prefill messages from {}: {}",
                path.display(),
                err
            );
            return Vec::new();
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                "Failed to parse prefill messages from {}: {}",
                path.display(),
                err
            );
            return Vec::new();
        }
    };
    if !value.is_array() {
        tracing::warn!(
            "Prefill messages file must contain a JSON array: {}",
            path.display()
        );
        return Vec::new();
    }
    match serde_json::from_value::<Vec<hermes_core::Message>>(value) {
        Ok(messages) => messages,
        Err(err) => {
            tracing::warn!(
                "Failed to parse prefill messages from {}: {}",
                path.display(),
                err
            );
            Vec::new()
        }
    }
}

include!("loader/env_bridges.rs");

#[cfg(test)]
mod tests;
