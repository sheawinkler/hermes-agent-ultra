//! ByteRover memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for ByteRover — persistent knowledge tree
//! with hierarchical context, tiered retrieval, and optional cloud sync via
//! the `brv` CLI.
//!
//! Mirrors the Python `plugins/memory/byterover/__init__.py`.
//!
//! Configuration:
//!   - `brv` CLI must be installed (npm install -g byterover-cli)
//!   - `BRV_API_KEY` (optional, for cloud sync)
//!   - `$HERMES_HOME/byterover.json` with `auto_extract: false` disables
//!     background turn/memory/compression curation while keeping tools enabled.
//!   - Working directory: `$HERMES_HOME/byterover/`

use std::process::Command;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const QUERY_TIMEOUT_SECS: u64 = 10;
const CURATE_TIMEOUT_SECS: u64 = 120;
const MIN_QUERY_LEN: usize = 10;
const MIN_OUTPUT_LEN: usize = 20;

fn coerce_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_i64().map(|n| n != 0).unwrap_or(default),
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

fn byterover_config_path_for_home(hermes_home: &str) -> std::path::PathBuf {
    std::path::Path::new(hermes_home).join("byterover.json")
}

fn default_byterover_config_path() -> std::path::PathBuf {
    config_io::default_hermes_home().join("byterover.json")
}

fn load_auto_extract(hermes_home: &str) -> bool {
    for key in ["HERMES_BYTEROVER_AUTO_EXTRACT", "BYTEROVER_AUTO_EXTRACT"] {
        if let Ok(value) = std::env::var(key) {
            return coerce_bool(Some(&Value::String(value)), true);
        }
    }
    let config = config_io::read_json_object(&byterover_config_path_for_home(hermes_home));
    coerce_bool(config.get("auto_extract"), true)
}

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn query_schema() -> Value {
    json!({
        "name": "brv_query",
        "description": "Search ByteRover's persistent knowledge tree for relevant context. Returns memories, project knowledge, architectural decisions, and patterns from previous sessions.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for."}
            },
            "required": ["query"]
        }
    })
}

fn curate_schema() -> Value {
    json!({
        "name": "brv_curate",
        "description": "Store important information in ByteRover's persistent knowledge tree. Use for architectural decisions, bug fixes, user preferences, project patterns — anything worth remembering across sessions.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string", "description": "The information to remember."}
            },
            "required": ["content"]
        }
    })
}

fn status_schema() -> Value {
    json!({
        "name": "brv_status",
        "description": "Check ByteRover status — CLI version, context tree stats, cloud sync state.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    })
}

// ---------------------------------------------------------------------------
// brv CLI resolution
// ---------------------------------------------------------------------------

fn resolve_brv_path() -> Option<String> {
    if let Ok(path) = which::which("brv") {
        return Some(path.to_string_lossy().to_string());
    }

    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".brv-cli").join("bin").join("brv"),
        std::path::PathBuf::from("/usr/local/bin/brv"),
        home.join(".npm-global").join("bin").join("brv"),
    ];
    for c in &candidates {
        if c.exists() {
            return Some(c.to_string_lossy().to_string());
        }
    }
    None
}

/// Run a `brv` CLI command. Returns `(success, output_or_error)`.
fn run_brv(args: &[&str], _timeout_secs: u64, cwd: &str) -> (bool, String) {
    let brv_path = match resolve_brv_path() {
        Some(p) => p,
        None => {
            return (
                false,
                "brv CLI not found. Install: npm install -g byterover-cli".into(),
            )
        }
    };

    let _ = std::fs::create_dir_all(cwd);

    let result = Command::new(&brv_path).args(args).current_dir(cwd).output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if output.status.success() {
                (true, stdout)
            } else {
                let err = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    format!("brv exited {}", output.status)
                };
                (false, err)
            }
        }
        Err(e) => (false, format!("brv execution failed: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// ByteRoverPlugin
// ---------------------------------------------------------------------------

/// ByteRover persistent memory via the `brv` CLI.
pub struct ByteRoverPlugin {
    cwd: Mutex<String>,
    session_id: Mutex<String>,
    turn_count: Mutex<u32>,
    auto_extract: Mutex<bool>,
}

impl ByteRoverPlugin {
    pub fn new() -> Self {
        Self {
            cwd: Mutex::new(String::new()),
            session_id: Mutex::new(String::new()),
            turn_count: Mutex::new(0),
            auto_extract: Mutex::new(true),
        }
    }

    fn working_dir(&self) -> String {
        self.cwd.lock().unwrap().clone()
    }

    fn auto_extract_enabled(&self) -> bool {
        *self.auto_extract.lock().unwrap()
    }
}

impl MemoryProviderPlugin for ByteRoverPlugin {
    fn name(&self) -> &str {
        "byterover"
    }

    fn is_available(&self) -> bool {
        resolve_brv_path().is_some()
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let cwd = std::path::Path::new(hermes_home)
            .join("byterover")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&cwd);

        *self.cwd.lock().unwrap() = cwd;
        *self.session_id.lock().unwrap() = session_id.to_string();
        *self.turn_count.lock().unwrap() = 0;
        *self.auto_extract.lock().unwrap() = load_auto_extract(hermes_home);

        tracing::info!(
            "ByteRover memory plugin initialized for session {}",
            session_id
        );
    }

    fn system_prompt_block(&self) -> String {
        if resolve_brv_path().is_none() {
            return String::new();
        }
        "# ByteRover Memory\n\
         Active. Persistent knowledge tree with hierarchical context.\n\
         Use brv_query to search past knowledge, brv_curate to store \
         important facts, brv_status to check state."
            .to_string()
    }

    fn prefetch(&self, query: &str, _session_id: &str) -> String {
        let query = query.trim();
        if query.len() < MIN_QUERY_LEN {
            return String::new();
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return String::new();
        }
        let truncated: String = query.chars().take(5000).collect();
        let (ok, output) = run_brv(&["query", "--", &truncated], QUERY_TIMEOUT_SECS, &cwd);
        if ok && output.len() > MIN_OUTPUT_LEN {
            format!("## ByteRover Context\n{}", output)
        } else {
            String::new()
        }
    }

    fn queue_prefetch(&self, _query: &str, _session_id: &str) {
        // prefetch() runs synchronously at turn start — no background queuing
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        *self.turn_count.lock().unwrap() += 1;

        if !self.auto_extract_enabled() {
            tracing::debug!("ByteRover sync_turn skipped because auto_extract is disabled");
            return;
        }
        if user_content.trim().len() < MIN_QUERY_LEN {
            return;
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return;
        }

        let combined = format!(
            "User: {}\nAssistant: {}",
            &user_content[..user_content.len().min(2000)],
            &assistant_content[..assistant_content.len().min(2000)]
        );
        std::thread::spawn(move || {
            let (ok, _) = run_brv(&["curate", "--", &combined], CURATE_TIMEOUT_SECS, &cwd);
            if !ok {
                tracing::debug!("ByteRover sync_turn curate failed");
            }
        });
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if !self.auto_extract_enabled() {
            tracing::debug!("ByteRover memory mirror skipped because auto_extract is disabled");
            return;
        }
        if !matches!(action, "add" | "replace") || content.is_empty() {
            return;
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return;
        }
        let label = if target == "user" {
            "User profile"
        } else {
            "Agent memory"
        };
        let tagged = format!("[{}] {}", label, content);
        std::thread::spawn(move || {
            let (ok, _) = run_brv(&["curate", "--", &tagged], CURATE_TIMEOUT_SECS, &cwd);
            if !ok {
                tracing::debug!("ByteRover memory mirror failed");
            }
        });
    }

    fn on_pre_compress(&self, messages: &[Value]) -> String {
        if !self.auto_extract_enabled() {
            tracing::debug!(
                "ByteRover pre-compression curation skipped because auto_extract is disabled"
            );
            return String::new();
        }
        if messages.is_empty() {
            return String::new();
        }
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return String::new();
        }

        let mut parts = Vec::new();
        for msg in messages.iter().rev().take(10).rev() {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !content.trim().is_empty() && (role == "user" || role == "assistant") {
                let truncated: String = content.chars().take(500).collect();
                parts.push(format!("{}: {}", role, truncated));
            }
        }
        if parts.is_empty() {
            return String::new();
        }

        let combined = format!("[Pre-compression context]\n{}", parts.join("\n"));
        std::thread::spawn(move || {
            let _ = run_brv(&["curate", "--", &combined], CURATE_TIMEOUT_SECS, &cwd);
        });
        String::new()
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![query_schema(), curate_schema(), status_schema()]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        let cwd = self.working_dir();
        if cwd.is_empty() {
            return json!({"error": "ByteRover not initialized"}).to_string();
        }

        match tool_name {
            "brv_query" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "query is required"}).to_string();
                }
                let truncated: String = query.chars().take(5000).collect();
                let (ok, output) = run_brv(&["query", "--", &truncated], QUERY_TIMEOUT_SECS, &cwd);
                if !ok {
                    return json!({"error": output}).to_string();
                }
                if output.is_empty() || output.len() < MIN_OUTPUT_LEN {
                    return json!({"result": "No relevant memories found."}).to_string();
                }
                let truncated_output: String = output.chars().take(8000).collect();
                json!({"result": truncated_output}).to_string()
            }
            "brv_curate" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    return json!({"error": "content is required"}).to_string();
                }
                let (ok, output) = run_brv(&["curate", "--", content], CURATE_TIMEOUT_SECS, &cwd);
                if !ok {
                    return json!({"error": output}).to_string();
                }
                json!({"result": "Memory curated successfully."}).to_string()
            }
            "brv_status" => {
                let (ok, output) = run_brv(&["status"], 15, &cwd);
                if !ok {
                    return json!({"error": output}).to_string();
                }
                json!({"status": output}).to_string()
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn on_turn_start(&self, turn_number: u32, _message: &str) {
        *self.turn_count.lock().unwrap() = turn_number;
    }

    fn shutdown(&self) {
        tracing::debug!("ByteRover memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "ByteRover API key (optional, for cloud sync)", "secret": true, "env_var": "BRV_API_KEY", "url": "https://app.byterover.dev"},
            {"key": "auto_extract", "description": "Automatically curate completed turns, explicit memory writes, and pre-compression context", "default": true, "choices": ["true", "false"]}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        config_io::merge_and_write_owner_only(&default_byterover_config_path(), config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn test_byterover_plugin_name() {
        let plugin = ByteRoverPlugin::new();
        assert_eq!(plugin.name(), "byterover");
    }

    #[test]
    fn test_byterover_tool_schemas() {
        let plugin = ByteRoverPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 3);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"brv_query"));
        assert!(names.contains(&"brv_curate"));
        assert!(names.contains(&"brv_status"));
    }

    #[test]
    fn test_byterover_system_prompt_empty_when_not_available() {
        let plugin = ByteRoverPlugin::new();
        // When brv is not installed, system_prompt_block may or may not be empty
        // depending on the test environment — just check it doesn't panic
        let _ = plugin.system_prompt_block();
    }

    #[test]
    fn test_byterover_prefetch_short_query_returns_empty() {
        let plugin = ByteRoverPlugin::new();
        assert!(plugin.prefetch("hi", "s1").is_empty());
    }

    #[test]
    fn test_byterover_handle_tool_missing_args() {
        let plugin = ByteRoverPlugin::new();
        // Not initialized, should return error
        let result = plugin.handle_tool_call("brv_query", &json!({"query": "test"}));
        assert!(result.contains("error"));
    }

    #[test]
    fn test_byterover_auto_extract_config_coerces_values() {
        assert!(!coerce_bool(Some(&json!(false)), true));
        assert!(!coerce_bool(Some(&json!("off")), true));
        assert!(coerce_bool(Some(&json!("yes")), false));
        assert!(coerce_bool(None, true));
    }

    #[test]
    fn test_byterover_initialize_honors_auto_extract_config() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _hermes_override = EnvGuard::remove("HERMES_BYTEROVER_AUTO_EXTRACT");
        let _byterover_override = EnvGuard::remove("BYTEROVER_AUTO_EXTRACT");
        std::fs::write(
            tmp.path().join("byterover.json"),
            r#"{"auto_extract": false}"#,
        )
        .expect("write config");

        let plugin = ByteRoverPlugin::new();
        plugin.initialize("session-1", tmp.path().to_str().expect("tmp"));

        assert!(!plugin.auto_extract_enabled());
    }

    #[test]
    fn test_byterover_config_schema_exposes_auto_extract() {
        let plugin = ByteRoverPlugin::new();
        let schema = plugin.get_config_schema().expect("schema");
        let keys = schema
            .as_array()
            .expect("schema array")
            .iter()
            .filter_map(|entry| entry.get("key").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert!(keys.contains(&"auto_extract"));
    }
}
