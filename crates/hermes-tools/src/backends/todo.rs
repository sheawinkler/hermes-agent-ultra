//! Real todo backend: JSON file-based task list.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Mutex;

use hermes_core::ToolError;
use crate::tools::todo::TodoBackend;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TodoItem {
    id: String,
    title: String,
    description: Option<String>,
    status: String,
    priority: String,
    created_at: String,
}

/// File-based todo backend storing tasks as JSON.
pub struct FileTodoBackend {
    file_path: std::path::PathBuf,
    items: Mutex<Vec<TodoItem>>,
}

impl FileTodoBackend {
    pub fn new(file_path: std::path::PathBuf) -> Self {
        let items = if file_path.exists() {
            std::fs::read_to_string(&file_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            file_path,
            items: Mutex::new(items),
        }
    }

    pub fn default_path() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let path = std::path::Path::new(&home).join(".hermes").join("todos.json");
        Self::new(path)
    }

    fn save(&self, items: &[TodoItem]) -> Result<(), ToolError> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::ExecutionFailed(format!("Failed to create dir: {}", e)))?;
        }
        let json = serde_json::to_string_pretty(items)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize todos: {}", e)))?;
        std::fs::write(&self.file_path, json)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write todos: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl TodoBackend for FileTodoBackend {
    async fn create(&self, title: &str, description: Option<&str>, priority: Option<&str>) -> Result<String, ToolError> {
        let mut items = self.items.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let id = uuid::Uuid::new_v4().to_string();
        let item = TodoItem {
            id: id.clone(),
            title: title.to_string(),
            description: description.map(String::from),
            status: "pending".to_string(),
            priority: priority.unwrap_or("medium").to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        items.push(item.clone());
        self.save(&items)?;
        Ok(json!({"status": "ok", "id": id, "title": title}).to_string())
    }

    async fn update(&self, id: &str, title: Option<&str>, description: Option<&str>, status: Option<&str>, priority: Option<&str>) -> Result<String, ToolError> {
        let mut items = self.items.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let item = items.iter_mut().find(|i| i.id == id)
            .ok_or_else(|| ToolError::NotFound(format!("Todo '{}' not found", id)))?;

        if let Some(t) = title { item.title = t.to_string(); }
        if let Some(d) = description { item.description = Some(d.to_string()); }
        if let Some(s) = status { item.status = s.to_string(); }
        if let Some(p) = priority { item.priority = p.to_string(); }

        self.save(&items)?;
        Ok(json!({"status": "ok", "id": id, "action": "updated"}).to_string())
    }

    async fn list(&self, status: Option<&str>) -> Result<String, ToolError> {
        let items = self.items.lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let filtered: Vec<&TodoItem> = match status {
            Some(s) => items.iter().filter(|i| i.status == s).collect(),
            None => items.iter().collect(),
        };
        let result: Vec<serde_json::Value> = filtered.iter().map(|i| {
            json!({
                "id": i.id,
                "title": i.title,
                "description": i.description,
                "status": i.status,
                "priority": i.priority,
                "created_at": i.created_at,
            })
        }).collect();
        Ok(json!({"todos": result, "total": result.len()}).to_string())
    }
}
