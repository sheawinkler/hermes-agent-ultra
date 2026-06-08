//! Shared debug session infrastructure for Hermes tools.
//!
//! Corresponds to `hermes-agent/tools/debug_helpers.py`.

use std::path::PathBuf;

use chrono::Utc;
use serde_json::{Map, Value, json};
use tracing::{debug, error};
use uuid::Uuid;

pub struct DebugSession {
    tool_name: String,
    enabled: bool,
    session_id: String,
    log_dir: PathBuf,
    calls: Vec<Value>,
    start_time: String,
}

impl DebugSession {
    pub fn new(tool_name: impl Into<String>, env_var: &str) -> Self {
        let enabled = std::env::var(env_var)
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self::with_enabled(
            tool_name,
            enabled,
            hermes_config::hermes_home().join("logs"),
        )
    }

    fn with_enabled(tool_name: impl Into<String>, enabled: bool, log_dir: PathBuf) -> Self {
        let tool_name = tool_name.into();
        let session_id = if enabled {
            Uuid::new_v4().to_string()
        } else {
            String::new()
        };
        let start_time = if enabled {
            Utc::now().to_rfc3339()
        } else {
            String::new()
        };

        if enabled {
            if let Err(err) = std::fs::create_dir_all(&log_dir) {
                error!("Error creating {} debug log dir: {}", tool_name, err);
            } else {
                debug!(
                    "{} debug mode enabled - Session ID: {}",
                    tool_name, session_id
                );
            }
        }

        Self {
            tool_name,
            enabled,
            session_id,
            log_dir,
            calls: Vec::new(),
            start_time,
        }
    }

    pub fn active(&self) -> bool {
        self.enabled
    }

    pub fn log_call(&mut self, call_name: &str, call_data: Value) {
        if !self.enabled {
            return;
        }

        let mut entry = Map::new();
        entry.insert("timestamp".to_string(), json!(Utc::now().to_rfc3339()));
        entry.insert("tool_name".to_string(), json!(call_name));
        if let Value::Object(map) = call_data {
            for (key, value) in map {
                entry.insert(key, value);
            }
        }
        self.calls.push(Value::Object(entry));
    }

    pub fn save(&self) {
        if !self.enabled {
            return;
        }

        let path = self.log_path();
        let payload = json!({
            "session_id": self.session_id,
            "start_time": self.start_time,
            "end_time": Utc::now().to_rfc3339(),
            "debug_enabled": true,
            "total_calls": self.calls.len(),
            "tool_calls": self.calls,
        });

        let result = serde_json::to_string_pretty(&payload)
            .map_err(|err| err.to_string())
            .and_then(|raw| std::fs::write(&path, raw).map_err(|err| err.to_string()));

        match result {
            Ok(()) => debug!("{} debug log saved: {}", self.tool_name, path.display()),
            Err(err) => error!("Error saving {} debug log: {}", self.tool_name, err),
        }
    }

    pub fn get_session_info(&self) -> Value {
        if !self.enabled {
            return json!({
                "enabled": false,
                "session_id": null,
                "log_path": null,
                "total_calls": 0,
            });
        }

        json!({
            "enabled": true,
            "session_id": self.session_id,
            "log_path": self.log_path(),
            "total_calls": self.calls.len(),
        })
    }

    pub fn log_path(&self) -> PathBuf {
        self.log_dir
            .join(format!("{}_debug_{}.json", self.tool_name, self.session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_session_is_noop_and_reports_inactive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut session = DebugSession::with_enabled("web_tools", false, temp.path().join("logs"));
        session.log_call("web_search", json!({"query": "rust"}));
        session.save();

        assert!(!session.active());
        assert_eq!(
            session.get_session_info(),
            json!({
                "enabled": false,
                "session_id": null,
                "log_path": null,
                "total_calls": 0,
            })
        );
    }

    #[test]
    fn enabled_session_logs_calls_and_saves_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_dir = temp.path().join("logs");

        let mut session = DebugSession::with_enabled("web_tools", true, log_dir);
        assert!(session.active());
        assert!(session.log_path().starts_with(temp.path().join("logs")));

        session.log_call("web_search", json!({"query": "rust", "results": 3}));
        session.save();

        let info = session.get_session_info();
        assert_eq!(info["enabled"], true);
        assert_eq!(info["total_calls"], 1);
        assert!(
            info["session_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );

        let raw = std::fs::read_to_string(session.log_path()).expect("read debug log");
        let saved: Value = serde_json::from_str(&raw).expect("debug json");
        assert_eq!(saved["debug_enabled"], true);
        assert_eq!(saved["total_calls"], 1);
        assert_eq!(saved["tool_calls"][0]["tool_name"], "web_search");
        assert_eq!(saved["tool_calls"][0]["query"], "rust");
        assert_eq!(saved["tool_calls"][0]["results"], 3);
    }
}
