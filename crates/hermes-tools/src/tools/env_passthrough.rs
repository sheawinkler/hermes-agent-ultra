use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use async_trait::async_trait;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde_json::{json, Value};

pub const SUBPROCESS_ENV_PASSTHROUGH_VAR: &str = "HERMES_SUBPROCESS_ENV_PASSTHROUGH";

static SESSION_PASSTHROUGH: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));
static CONFIG_PASSTHROUGH: LazyLock<Mutex<Option<BTreeSet<String>>>> =
    LazyLock::new(|| Mutex::new(None));

pub struct EnvPassthroughHandler;

fn normalize_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn hermes_home() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".hermes")))
}

fn mapping_get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.get(serde_yaml::Value::String(key.to_string()))
}

fn load_config_passthrough_uncached() -> BTreeSet<String> {
    let Some(home) = hermes_home() else {
        return BTreeSet::new();
    };
    let path = home.join("config.yaml");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    let Ok(root) = serde_yaml::from_str::<serde_yaml::Value>(&raw) else {
        return BTreeSet::new();
    };
    let serde_yaml::Value::Mapping(root) = root else {
        return BTreeSet::new();
    };
    let Some(serde_yaml::Value::Mapping(terminal)) = mapping_get(&root, "terminal") else {
        return BTreeSet::new();
    };
    let Some(serde_yaml::Value::Sequence(values)) = mapping_get(terminal, "env_passthrough") else {
        return BTreeSet::new();
    };

    values
        .iter()
        .filter_map(serde_yaml::Value::as_str)
        .filter_map(normalize_name)
        .collect()
}

fn config_passthrough() -> BTreeSet<String> {
    let mut guard = CONFIG_PASSTHROUGH.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = guard.as_ref() {
        return cached.clone();
    }
    let loaded = load_config_passthrough_uncached();
    *guard = Some(loaded.clone());
    loaded
}

fn sync_process_env_from_set(values: &BTreeSet<String>) {
    if values.is_empty() {
        std::env::remove_var(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    } else {
        std::env::set_var(
            SUBPROCESS_ENV_PASSTHROUGH_VAR,
            values.iter().cloned().collect::<Vec<_>>().join(" "),
        );
    }
}

pub fn register_env_passthrough<I, S>(var_names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    {
        let mut allowed = SESSION_PASSTHROUGH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for name in var_names
            .into_iter()
            .filter_map(|v| normalize_name(v.as_ref()))
        {
            allowed.insert(name);
        }
    }
    sync_process_env_from_set(&get_all_passthrough());
}

pub fn clear_env_passthrough() {
    SESSION_PASSTHROUGH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    sync_process_env_from_set(&get_all_passthrough());
}

pub fn is_env_passthrough(var_name: &str) -> bool {
    let Some(name) = normalize_name(var_name) else {
        return false;
    };
    if SESSION_PASSTHROUGH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&name)
    {
        return true;
    }
    config_passthrough().contains(&name)
}

pub fn get_all_passthrough() -> BTreeSet<String> {
    let mut all = config_passthrough();
    all.extend(
        SESSION_PASSTHROUGH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned(),
    );
    all
}

#[cfg(test)]
pub fn reset_config_passthrough_cache_for_tests() {
    *CONFIG_PASSTHROUGH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    sync_process_env_from_set(&get_all_passthrough());
}

#[async_trait]
impl ToolHandler for EnvPassthroughHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read");

        match action {
            "register" => {
                let keys = params
                    .get("keys")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'keys'".into()))?;
                let names: Vec<String> = keys
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(normalize_name)
                    .collect();
                register_env_passthrough(names.iter());
                Ok(json!({"status":"registered","keys":names}).to_string())
            }
            "clear" => {
                clear_env_passthrough();
                Ok(json!({"status":"cleared"}).to_string())
            }
            "list" => Ok(json!({"keys": get_all_passthrough()}).to_string()),
            "read" | "get" => {
                let keys = params
                    .get("keys")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'keys'".into()))?;

                let mut out = serde_json::Map::new();
                for key in keys
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(normalize_name)
                {
                    if let Ok(value) = std::env::var(&key) {
                        out.insert(key, json!(value));
                    }
                }
                Ok(Value::Object(out).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unsupported env_passthrough action '{other}'"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({"type":"string","enum":["read","get","list","register","clear"]}),
        );
        props.insert(
            "keys".into(),
            json!({"type":"array","items":{"type":"string"}}),
        );
        tool_schema(
            "env_passthrough",
            "Expose selected env vars to tool workflows and register vars allowed through subprocess sanitizers.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn reset() {
        SESSION_PASSTHROUGH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *CONFIG_PASSTHROUGH.lock().unwrap_or_else(|e| e.into_inner()) = None;
        std::env::remove_var(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    }

    #[test]
    fn registry_trims_skips_empty_and_clears_session_names() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();

        assert!(!is_env_passthrough("TENOR_API_KEY"));
        register_env_passthrough(["  TENOR_API_KEY  ", "", "  ", "BAR_SECRET"]);
        assert!(is_env_passthrough("TENOR_API_KEY"));
        assert!(is_env_passthrough("BAR_SECRET"));
        assert!(!is_env_passthrough(""));
        assert_eq!(
            std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR).unwrap(),
            "BAR_SECRET TENOR_API_KEY"
        );

        clear_env_passthrough();
        assert!(!is_env_passthrough("TENOR_API_KEY"));
        assert!(std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR).is_err());
    }

    #[test]
    fn config_passthrough_is_loaded_from_hermes_home() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("config.yaml"),
            "terminal:\n  env_passthrough:\n    - MY_CUSTOM_KEY\n    - '  ANOTHER_TOKEN  '\n",
        )
        .expect("write config");
        std::env::set_var("HERMES_HOME", tmp.path());
        reset_config_passthrough_cache_for_tests();

        assert!(is_env_passthrough("MY_CUSTOM_KEY"));
        assert!(is_env_passthrough("ANOTHER_TOKEN"));
        assert!(!is_env_passthrough("UNRELATED_VAR"));

        register_env_passthrough(["SKILL_KEY"]);
        let all = get_all_passthrough();
        assert!(all.contains("MY_CUSTOM_KEY"));
        assert!(all.contains("SKILL_KEY"));
        assert_eq!(
            std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR).unwrap(),
            "ANOTHER_TOKEN MY_CUSTOM_KEY SKILL_KEY"
        );

        std::env::remove_var("HERMES_HOME");
        reset();
    }
}
