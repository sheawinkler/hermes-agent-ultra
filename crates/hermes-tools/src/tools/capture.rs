//! Capture inbox — quick fragment storage + optional reminder hints (Python memory+cron pattern).

use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use indexmap::IndexMap;
use serde_json::{json, Value};
use uuid::Uuid;

use hermes_config::paths::hermes_home;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

fn inbox_dir() -> PathBuf {
    hermes_home().join("inbox")
}

fn list_inbox_files() -> Result<Vec<PathBuf>, ToolError> {
    let dir = inbox_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| ToolError::ExecutionFailed(format!("read inbox dir: {e}")))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    files.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
    files.reverse();
    Ok(files)
}

pub struct CaptureHandler;

#[async_trait]
impl ToolHandler for CaptureHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("save");

        match action {
            "list" => {
                let files = list_inbox_files()?;
                let items: Vec<Value> = files
                    .iter()
                    .take(50)
                    .filter_map(|path| {
                        let id = path.file_stem()?.to_str()?.to_string();
                        let content = fs::read_to_string(path).ok()?;
                        Some(json!({
                            "id": id,
                            "path": path.display().to_string(),
                            "preview": content.chars().take(200).collect::<String>(),
                        }))
                    })
                    .collect();
                Ok(json!({"items": items, "count": items.len()}).to_string())
            }
            "save" => {
                let content = params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content'".into()))?
                    .trim();
                if content.is_empty() {
                    return Err(ToolError::InvalidParams("'content' cannot be empty".into()));
                }
                let tags = params
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let remind_at = params
                    .get("remind_at")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);

                let id = Uuid::new_v4().simple().to_string();
                let dir = inbox_dir();
                fs::create_dir_all(&dir)
                    .map_err(|e| ToolError::ExecutionFailed(format!("create inbox: {e}")))?;
                let path = dir.join(format!("{id}.md"));
                let created = Utc::now().to_rfc3339();
                let mut body = String::from("---\n");
                body.push_str(&format!("id: {id}\n"));
                body.push_str(&format!("created_at: {created}\n"));
                if !tags.is_empty() {
                    body.push_str(&format!("tags: [{}]\n", tags.join(", ")));
                }
                if let Some(schedule) = remind_at.as_deref() {
                    body.push_str(&format!("remind_at: {schedule}\n"));
                }
                body.push_str("---\n\n");
                body.push_str(content);
                fs::write(&path, body)
                    .map_err(|e| ToolError::ExecutionFailed(format!("write inbox item: {e}")))?;

                let mut response = json!({
                    "id": id,
                    "path": path.display().to_string(),
                    "saved": true,
                });
                if let Some(schedule) = remind_at {
                    response["cron_hint"] = json!({
                        "action": "create",
                        "schedule": schedule,
                        "task": format!("Reminder for captured note {id}: {content}"),
                        "deliver": "origin",
                        "note": "Call cronjob to schedule; deliver omitted auto-targets current chat"
                    });
                }
                Ok(response.to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action '{other}'. Use 'save' or 'list'."
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({"type":"string","enum":["save","list"],"default":"save"}),
        );
        props.insert(
            "content".into(),
            json!({"type":"string","description":"Note body for action=save"}),
        );
        props.insert(
            "tags".into(),
            json!({"type":"array","items":{"type":"string"},"description":"Optional tags"}),
        );
        props.insert(
            "remind_at".into(),
            json!({"type":"string","description":"Optional natural-language schedule for cronjob (e.g. 30m, every 2h)"}),
        );
        tool_schema(
            "capture",
            "Save a quick note to ~/.hermes/inbox and optionally get a cronjob hint for reminders.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_and_list_capture() {
        let prev = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        unsafe {
            std::env::set_var("HERMES_HOME", tmp.path());
        }
        let handler = CaptureHandler;
        let saved = handler
            .execute(json!({"action":"save","content":"buy milk","tags":["errands"]}))
            .await
            .expect("save");
        let saved_v: Value = serde_json::from_str(&saved).expect("json");
        assert_eq!(saved_v["saved"], true);
        let listed = handler.execute(json!({"action":"list"})).await.expect("list");
        let listed_v: Value = serde_json::from_str(&listed).expect("json");
        assert!(listed_v["count"].as_u64().unwrap_or(0) >= 1);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }
}
