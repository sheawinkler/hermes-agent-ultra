//! File-based todo backend — aligned with Python todo_tool.py.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tools::todo::{dedupe_by_id, validate_item, TodoBackend, TodoItem, VALID_STATUSES};
use hermes_core::ToolError;

// ---------------------------------------------------------------------------
// Internal storage type (adds created_at for persistence metadata)
// ---------------------------------------------------------------------------

/// Stored representation — superset of `TodoItem` with a `created_at` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredItem {
    id: String,
    content: String,
    status: String,
    #[serde(default)]
    created_at: String,
}

impl From<StoredItem> for TodoItem {
    fn from(s: StoredItem) -> Self {
        TodoItem { id: s.id, content: s.content, status: s.status }
    }
}

impl From<TodoItem> for StoredItem {
    fn from(t: TodoItem) -> Self {
        StoredItem {
            id: t.id,
            content: t.content,
            status: t.status,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// FileTodoBackend
// ---------------------------------------------------------------------------

/// File-based todo backend storing tasks as JSON.
///
/// Default path: `~/.hermes/todos.json`
pub struct FileTodoBackend {
    file_path: std::path::PathBuf,
    items: Mutex<Vec<StoredItem>>,
}

impl FileTodoBackend {
    /// Create a backend backed by the given file path.
    /// Existing content is loaded on construction.
    pub fn new(file_path: std::path::PathBuf) -> Self {
        let items = if file_path.exists() {
            std::fs::read_to_string(&file_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { file_path, items: Mutex::new(items) }
    }

    /// Create a backend using the default `~/.hermes/todos.json` path.
    pub fn default_path() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        let path = std::path::Path::new(&home).join(".hermes").join("todos.json");
        Self::new(path)
    }

    fn save(&self, items: &[StoredItem]) -> Result<(), ToolError> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to create dir: {e}"))
            })?;
        }
        let json = serde_json::to_string_pretty(items).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to serialize todos: {e}"))
        })?;
        std::fs::write(&self.file_path, json).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to write todos: {e}"))
        })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TodoBackend implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl TodoBackend for FileTodoBackend {
    async fn read(&self) -> Result<Vec<TodoItem>, ToolError> {
        let items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(items.iter().cloned().map(TodoItem::from).collect())
    }

    async fn write_all(&self, todos: Vec<TodoItem>) -> Result<Vec<TodoItem>, ToolError> {
        // Dedup by id (keep last) then validate each item
        let validated: Vec<TodoItem> =
            dedupe_by_id(todos).into_iter().map(validate_item).collect();
        let stored: Vec<StoredItem> = validated.iter().cloned().map(StoredItem::from).collect();

        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        *items = stored;
        self.save(&items)?;
        Ok(validated)
    }

    async fn merge_items(&self, todos: Vec<TodoItem>) -> Result<Vec<TodoItem>, ToolError> {
        // Dedup incoming list first
        let deduped = dedupe_by_id(todos);

        let mut items = self
            .items
            .lock()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        // Build id → index map for O(1) lookups
        let mut id_to_idx: HashMap<String, usize> = items
            .iter()
            .enumerate()
            .map(|(i, item)| (item.id.clone(), i))
            .collect();

        for t in &deduped {
            let id = t.id.trim().to_string();
            if id.is_empty() {
                continue; // Cannot merge an item without an id
            }

            if let Some(&idx) = id_to_idx.get(&id) {
                // Update existing: only non-empty content and valid status values are applied
                if !t.content.trim().is_empty() {
                    items[idx].content = t.content.trim().to_string();
                }
                let status = t.status.trim().to_lowercase();
                if VALID_STATUSES.contains(&status.as_str()) {
                    items[idx].status = status;
                }
            } else {
                // New item — validate fully and append
                let validated = validate_item(t.clone());
                let stored = StoredItem::from(validated.clone());
                id_to_idx.insert(stored.id.clone(), items.len());
                items.push(stored);
            }
        }

        self.save(&items)?;
        Ok(items.iter().cloned().map(TodoItem::from).collect())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_backend() -> (FileTodoBackend, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todos.json");
        (FileTodoBackend::new(path), dir)
    }

    fn item(id: &str, content: &str, status: &str) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            content: content.to_string(),
            status: status.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // 8. Persistence: write survives a reload from disk
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_persistence_write_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todos.json");

        {
            let backend = FileTodoBackend::new(path.clone());
            backend
                .write_all(vec![item("p1", "Persisted task", "pending")])
                .await
                .unwrap();
        }

        // Reload from the same path — items must survive the round-trip
        let backend2 = FileTodoBackend::new(path);
        let items = backend2.read().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "p1");
        assert_eq!(items[0].content, "Persisted task");
        assert_eq!(items[0].status, "pending");
    }

    // write_all replaces the entire list
    #[tokio::test]
    async fn test_write_all_replaces_list() {
        let (backend, _dir) = make_backend();
        backend
            .write_all(vec![item("a", "Task A", "pending")])
            .await
            .unwrap();
        let result = backend
            .write_all(vec![item("b", "Task B", "in_progress")])
            .await
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "b");
        assert_eq!(result[0].status, "in_progress");
    }

    // merge_items updates existing and appends new
    #[tokio::test]
    async fn test_merge_items_backend() {
        let (backend, _dir) = make_backend();
        backend
            .write_all(vec![item("x", "X task", "pending")])
            .await
            .unwrap();

        let result = backend
            .merge_items(vec![
                item("x", "X task", "completed"),
                item("y", "Y task", "pending"),
            ])
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "x");
        assert_eq!(result[0].status, "completed");
        assert_eq!(result[1].id, "y");
        assert_eq!(result[1].status, "pending");
    }

    // merge skips items with empty id
    #[tokio::test]
    async fn test_merge_skips_empty_id() {
        let (backend, _dir) = make_backend();
        backend
            .write_all(vec![item("k1", "Keep me", "pending")])
            .await
            .unwrap();

        let result = backend
            .merge_items(vec![item("", "Should be skipped", "pending")])
            .await
            .unwrap();

        // The empty-id item must not be appended (can't merge without id)
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "k1");
    }

    // file is created in a non-existing directory
    #[tokio::test]
    async fn test_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("dir").join("todos.json");
        let backend = FileTodoBackend::new(path.clone());
        backend
            .write_all(vec![item("d1", "Dir test", "pending")])
            .await
            .unwrap();
        assert!(path.exists());
    }
}
