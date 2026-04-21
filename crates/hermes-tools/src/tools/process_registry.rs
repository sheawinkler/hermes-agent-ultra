use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

#[derive(Clone, Debug)]
struct ProcessEntry {
    name: String,
    pid: i64,
    command: Option<String>,
    task_id: Option<String>,
    session_key: Option<String>,
    status: String,
    started_at: i64,
    updated_at: i64,
}

#[derive(Clone, Default)]
pub struct ProcessRegistryHandler {
    entries: Arc<Mutex<HashMap<String, ProcessEntry>>>,
}

fn unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn normalize_opt_string(input: Option<&Value>) -> Option<String> {
    let raw = input.and_then(Value::as_str)?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

fn parse_status(input: Option<&Value>) -> String {
    let raw = input
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "running".to_string());
    match raw.as_str() {
        "running" | "exited" | "stopped" | "failed" => raw,
        _ => "running".to_string(),
    }
}

fn parse_name(params: &Value) -> Result<String, ToolError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() {
        return Err(ToolError::InvalidParams(
            "action requires non-empty 'name'".into(),
        ));
    }
    Ok(name.to_string())
}

fn serialize_entry(entry: &ProcessEntry) -> Value {
    json!({
        "name": entry.name,
        "pid": entry.pid,
        "command": entry.command,
        "task_id": entry.task_id,
        "session_key": entry.session_key,
        "status": entry.status,
        "started_at": entry.started_at,
        "updated_at": entry.updated_at,
    })
}

#[async_trait]
impl ToolHandler for ProcessRegistryHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");
        match action {
            "register" => {
                let name = parse_name(&params)?;
                let pid = params.get("pid").and_then(|v| v.as_i64()).unwrap_or(0);
                if pid <= 0 {
                    return Err(ToolError::InvalidParams(
                        "register requires positive pid".into(),
                    ));
                }
                let now = unix_ts();
                let entry = ProcessEntry {
                    name: name.clone(),
                    pid,
                    command: normalize_opt_string(params.get("command")),
                    task_id: normalize_opt_string(params.get("task_id")),
                    session_key: normalize_opt_string(params.get("session_key")),
                    status: parse_status(params.get("status")),
                    started_at: params
                        .get("started_at")
                        .and_then(Value::as_i64)
                        .unwrap_or(now),
                    updated_at: now,
                };
                self.entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .insert(name.clone(), entry);
                Ok(json!({"status":"registered","name":name,"pid":pid}).to_string())
            }
            "update" => {
                let name = parse_name(&params)?;
                let mut entries = self
                    .entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let Some(existing) = entries.get_mut(&name) else {
                    return Ok(json!({"status":"not_found","name":name}).to_string());
                };

                if let Some(pid) = params.get("pid").and_then(Value::as_i64) {
                    if pid <= 0 {
                        return Err(ToolError::InvalidParams("pid must be positive".into()));
                    }
                    existing.pid = pid;
                }
                if params.get("status").is_some() {
                    existing.status = parse_status(params.get("status"));
                }
                if params.get("command").is_some() {
                    existing.command = normalize_opt_string(params.get("command"));
                }
                if params.get("task_id").is_some() {
                    existing.task_id = normalize_opt_string(params.get("task_id"));
                }
                if params.get("session_key").is_some() {
                    existing.session_key = normalize_opt_string(params.get("session_key"));
                }
                existing.updated_at = unix_ts();
                Ok(json!({"status":"updated","entry": serialize_entry(existing)}).to_string())
            }
            "get" | "poll" => {
                let name = parse_name(&params)?;
                let entries = self
                    .entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                match entries.get(&name) {
                    Some(entry) => {
                        Ok(json!({"status":"ok","entry": serialize_entry(entry)}).to_string())
                    }
                    None => Ok(json!({"status":"not_found","name":name}).to_string()),
                }
            }
            "deregister" | "remove" => {
                let name = parse_name(&params)?;
                let removed = self
                    .entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .remove(&name)
                    .is_some();
                Ok(
                    json!({"status": if removed {"removed"} else {"not_found"}, "name": name})
                        .to_string(),
                )
            }
            "clear" => {
                let removed = self
                    .entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .drain()
                    .count();
                Ok(json!({"status":"cleared","removed": removed}).to_string())
            }
            _ => {
                let entries = self
                    .entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let ordered: BTreeMap<String, Value> = entries
                    .iter()
                    .map(|(k, v)| (k.clone(), serialize_entry(v)))
                    .collect();
                Ok(json!({"status":"ok","entries": ordered, "count": ordered.len()}).to_string())
            }
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({"type":"string","enum":["list","register","update","get","poll","deregister","remove","clear"]}),
        );
        props.insert("name".into(), json!({"type":"string"}));
        props.insert("pid".into(), json!({"type":"integer"}));
        props.insert("command".into(), json!({"type":"string"}));
        props.insert("task_id".into(), json!({"type":"string"}));
        props.insert("session_key".into(), json!({"type":"string"}));
        props.insert(
            "status".into(),
            json!({"type":"string","enum":["running","exited","stopped","failed"]}),
        );
        props.insert("started_at".into(), json!({"type":"integer"}));
        tool_schema(
            "process_registry",
            "Manage lightweight process metadata entries for background tasks.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_json(raw: &str) -> Value {
        serde_json::from_str(raw).expect("valid json output")
    }

    #[tokio::test]
    async fn register_get_update_remove_roundtrip() {
        let handler = ProcessRegistryHandler::default();

        let out = handler
            .execute(json!({
                "action":"register",
                "name":"proc_a",
                "pid":1234,
                "command":"pytest -q",
                "task_id":"task_1",
                "session_key":"gw_1"
            }))
            .await
            .expect("register");
        let parsed = parse_json(&out);
        assert_eq!(parsed["status"], "registered");

        let out = handler
            .execute(json!({"action":"get","name":"proc_a"}))
            .await
            .expect("get");
        let parsed = parse_json(&out);
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["entry"]["pid"], 1234);
        assert_eq!(parsed["entry"]["command"], "pytest -q");

        let out = handler
            .execute(json!({"action":"update","name":"proc_a","status":"exited","pid":4321}))
            .await
            .expect("update");
        let parsed = parse_json(&out);
        assert_eq!(parsed["status"], "updated");
        assert_eq!(parsed["entry"]["status"], "exited");
        assert_eq!(parsed["entry"]["pid"], 4321);

        let out = handler
            .execute(json!({"action":"remove","name":"proc_a"}))
            .await
            .expect("remove");
        let parsed = parse_json(&out);
        assert_eq!(parsed["status"], "removed");
    }

    #[tokio::test]
    async fn list_and_clear_entries() {
        let handler = ProcessRegistryHandler::default();

        for (name, pid) in [("proc_a", 11), ("proc_b", 22)] {
            handler
                .execute(json!({"action":"register","name":name,"pid":pid}))
                .await
                .expect("register");
        }

        let out = handler
            .execute(json!({"action":"list"}))
            .await
            .expect("list");
        let parsed = parse_json(&out);
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["count"], 2);

        let out = handler
            .execute(json!({"action":"clear"}))
            .await
            .expect("clear");
        let parsed = parse_json(&out);
        assert_eq!(parsed["status"], "cleared");
        assert_eq!(parsed["removed"], 2);
    }

    #[tokio::test]
    async fn invalid_register_inputs_are_rejected() {
        let handler = ProcessRegistryHandler::default();
        let err = handler
            .execute(json!({"action":"register","name":"", "pid": 1}))
            .await
            .expect_err("expected invalid name");
        assert!(err.to_string().contains("non-empty"));

        let err = handler
            .execute(json!({"action":"register","name":"proc", "pid": 0}))
            .await
            .expect_err("expected invalid pid");
        assert!(err.to_string().contains("positive pid"));
    }
}
