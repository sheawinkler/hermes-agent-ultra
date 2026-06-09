use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const DEFAULT_HOME_DIR: &str = ".hermes-agent-ultra";
const LEGACY_HOME_DIR: &str = ".hermes";

pub(crate) fn default_hermes_home() -> PathBuf {
    if let Some(home) = env_var_path("HERMES_HOME") {
        return home;
    }
    if let Some(home) = env_var_path("HERMES_AGENT_ULTRA_HOME") {
        return home;
    }

    let home_dir = user_home_dir();
    let primary = home_dir.join(DEFAULT_HOME_DIR);
    let legacy = home_dir.join(LEGACY_HOME_DIR);
    if primary.exists() || !legacy.exists() {
        primary
    } else {
        legacy
    }
}

fn env_var_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn user_home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("USERPROFILE") {
        PathBuf::from(home)
    } else {
        PathBuf::from(".")
    }
}

pub(crate) fn read_json_object(path: &Path) -> Map<String, Value> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Map::new();
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

pub(crate) fn json_file_has_nonempty_string(path: &Path, keys: &[&str]) -> bool {
    let object = read_json_object(path);
    keys.iter().any(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

pub(crate) fn merge_and_write_owner_only(path: &Path, values: &Value) -> Result<(), String> {
    let incoming = values
        .as_object()
        .ok_or_else(|| "config must be a JSON object".to_string())?;
    let mut existing = read_json_object(path);
    merge_object_values(&mut existing, incoming);
    write_owner_only_atomic(path, &Value::Object(existing))
}

fn merge_object_values(existing: &mut Map<String, Value>, incoming: &Map<String, Value>) {
    for (key, value) in incoming {
        match (existing.get_mut(key), value) {
            (Some(Value::Object(existing_child)), Value::Object(incoming_child)) => {
                merge_object_values(existing_child, incoming_child);
            }
            _ => {
                existing.insert(key.clone(), value.clone());
            }
        }
    }
}

pub(crate) fn write_owner_only_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("config path {} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp_path = parent.join(format!(".{file_name}.tmp.{}.{}", std::process::id(), nonce));
    let raw = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize config: {e}"))?;

    let result = (|| -> Result<(), String> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options
            .open(&tmp_path)
            .map_err(|e| format!("create {}: {e}", tmp_path.display()))?;
        file.write_all(&raw)
            .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
        file.write_all(b"\n")
            .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("fsync {}: {e}", tmp_path.display()))?;
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("chmod {}: {e}", tmp_path.display()))?;
        }

        fs::rename(&tmp_path, path)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp_path.display(), path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("chmod {}: {e}", path.display()))?;
        }

        let _ = fs::File::open(parent).and_then(|dir| dir.sync_all());
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn owner_only_atomic_write_creates_private_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("secret.json");

        write_owner_only_atomic(&path, &json!({"api_key": "secret"})).expect("write config");

        let parsed: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(parsed["api_key"], "secret");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn default_home_matches_ultra_home_precedence() {
        let _guard = TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::remove("HERMES_HOME");
        let _ultra = EnvGuard::remove("HERMES_AGENT_ULTRA_HOME");
        let _userprofile = EnvGuard::remove("USERPROFILE");
        let _home_dir = EnvGuard::set("HOME", tmp.path());

        assert_eq!(default_hermes_home(), tmp.path().join(DEFAULT_HOME_DIR));

        let ultra_override = tmp.path().join("custom-ultra-home");
        let _ultra = EnvGuard::set("HERMES_AGENT_ULTRA_HOME", &ultra_override);
        assert_eq!(default_hermes_home(), ultra_override);
    }
}
