//! Todo management tool — aligned with Python todo_tool.py external contract.
//!
//! Contract:
//! - `todos` absent / null  → read mode (return current list)
//! - `merge=false` (default) → full replace (dedup by id, keep last; then validate)
//! - `merge=true`            → update existing by id, append new items
//!
//! Return: `{"todos":[…], "summary":{"total":N,"pending":N,"in_progress":N,"completed":N,"cancelled":N}}`

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// Public types shared with the backend
// ---------------------------------------------------------------------------

/// A single todo item (external contract: id, content, status only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Constants / helpers — pub(crate) so backends/todo.rs can reuse them
// ---------------------------------------------------------------------------

pub(crate) const VALID_STATUSES: &[&str] =
    &["pending", "in_progress", "completed", "cancelled"];

/// Validate and normalise a `TodoItem`. Mirrors Python `TodoStore._validate`.
///
/// - empty id       → `"?"`
/// - empty content  → `"(no description)"`
/// - invalid status → `"pending"`
pub(crate) fn validate_item(item: TodoItem) -> TodoItem {
    let id = item.id.trim().to_string();
    let id = if id.is_empty() { "?".to_string() } else { id };

    let content = item.content.trim().to_string();
    let content = if content.is_empty() {
        "(no description)".to_string()
    } else {
        content
    };

    let status = item.status.trim().to_lowercase();
    let status = if VALID_STATUSES.contains(&status.as_str()) {
        status
    } else {
        "pending".to_string()
    };

    TodoItem { id, content, status }
}

/// Deduplicate by id, keeping the **last** occurrence at its sorted-index position.
/// Mirrors Python `TodoStore._dedupe_by_id`.
pub(crate) fn dedupe_by_id(todos: Vec<TodoItem>) -> Vec<TodoItem> {
    let mut last_index: HashMap<String, usize> = HashMap::new();
    for (i, item) in todos.iter().enumerate() {
        last_index.insert(norm_id(&item.id), i);
    }
    todos
        .into_iter()
        .enumerate()
        .filter_map(|(i, item)| {
            if last_index.get(&norm_id(&item.id)) == Some(&i) {
                Some(item)
            } else {
                None
            }
        })
        .collect()
}

#[inline]
fn norm_id(id: &str) -> String {
    let t = id.trim();
    if t.is_empty() { "?".to_string() } else { t.to_string() }
}

// ---------------------------------------------------------------------------
// TodoBackend trait
// ---------------------------------------------------------------------------

/// Backend for todo/task list management (Python-aligned contract).
#[async_trait]
pub trait TodoBackend: Send + Sync {
    /// Return all current todo items.
    async fn read(&self) -> Result<Vec<TodoItem>, ToolError>;

    /// Replace the entire list (dedup by id, then validate each item).
    async fn write_all(&self, todos: Vec<TodoItem>) -> Result<Vec<TodoItem>, ToolError>;

    /// Merge: update existing items by id; validate and append new ones.
    async fn merge_items(&self, todos: Vec<TodoItem>) -> Result<Vec<TodoItem>, ToolError>;
}

// ---------------------------------------------------------------------------
// Private helpers for the handler
// ---------------------------------------------------------------------------

/// Parse a raw JSON value into a `TodoItem` with sensible defaults.
fn parse_raw_item(v: &Value) -> TodoItem {
    TodoItem {
        id: v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        content: v
            .get("content")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        status: v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("pending")
            .to_string(),
    }
}

/// Build the standard JSON response. Mirrors Python `todo_tool()` return format.
fn build_response(items: &[TodoItem]) -> String {
    let pending = items.iter().filter(|i| i.status == "pending").count();
    let in_progress = items.iter().filter(|i| i.status == "in_progress").count();
    let completed = items.iter().filter(|i| i.status == "completed").count();
    let cancelled = items.iter().filter(|i| i.status == "cancelled").count();

    json!({
        "todos": items,
        "summary": {
            "total": items.len(),
            "pending": pending,
            "in_progress": in_progress,
            "completed": completed,
            "cancelled": cancelled,
        }
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// TodoHandler
// ---------------------------------------------------------------------------

/// Tool for managing a task/todo list.
pub struct TodoHandler {
    backend: Arc<dyn TodoBackend>,
}

impl TodoHandler {
    pub fn new(backend: Arc<dyn TodoBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for TodoHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let todos_val = params.get("todos");
        let merge = params
            .get("merge")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let items = match todos_val {
            // todos absent or explicitly null → read mode
            None | Some(Value::Null) => self.backend.read().await?,
            Some(val) => {
                let arr = val.as_array().ok_or_else(|| {
                    ToolError::InvalidParams("'todos' must be an array".into())
                })?;
                let todos: Vec<TodoItem> = arr.iter().map(parse_raw_item).collect();
                if merge {
                    self.backend.merge_items(todos).await?
                } else {
                    self.backend.write_all(todos).await?
                }
            }
        };

        Ok(build_response(&items))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "todos".into(),
            json!({
                "type": "array",
                "description": "Task items to write. Omit to read current list.",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Unique item identifier"
                        },
                        "content": {
                            "type": "string",
                            "description": "Task description"
                        },
                        "status": {
                            "type": "string",
                            "enum": ["pending", "in_progress", "completed", "cancelled"],
                            "description": "Current status"
                        }
                    },
                    "required": ["id", "content", "status"]
                }
            }),
        );
        props.insert(
            "merge".into(),
            json!({
                "type": "boolean",
                "description": "true: update existing items by id, add new ones. false (default): replace the entire list.",
                "default": false
            }),
        );

        tool_schema(
            "todo",
            concat!(
                "Manage your task list for the current session. ",
                "Use for complex tasks with 3+ steps or when the user provides multiple tasks. ",
                "Call with no parameters to read the current list.\n\n",
                "Writing:\n",
                "- Provide 'todos' array to create/update items\n",
                "- merge=false (default): replace the entire list with a fresh plan\n",
                "- merge=true: update existing items by id, add any new ones\n\n",
                "Each item: {id: string, content: string, ",
                "status: pending|in_progress|completed|cancelled}\n",
                "List order is priority. Only ONE item in_progress at a time.\n",
                "Mark items completed immediately when done. ",
                "If something fails, cancel it and add a revised item.\n\n",
                "Always returns the full current list."
            ),
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // -----------------------------------------------------------------------
    // In-memory mock backend (mirrors FileTodoBackend logic for unit tests)
    // -----------------------------------------------------------------------

    struct MockTodoBackend {
        items: Mutex<Vec<TodoItem>>,
    }

    impl MockTodoBackend {
        fn new() -> Self {
            Self { items: Mutex::new(Vec::new()) }
        }
    }

    #[async_trait]
    impl TodoBackend for MockTodoBackend {
        async fn read(&self) -> Result<Vec<TodoItem>, ToolError> {
            Ok(self.items.lock().unwrap().clone())
        }

        async fn write_all(&self, todos: Vec<TodoItem>) -> Result<Vec<TodoItem>, ToolError> {
            let validated: Vec<TodoItem> =
                dedupe_by_id(todos).into_iter().map(validate_item).collect();
            *self.items.lock().unwrap() = validated.clone();
            Ok(validated)
        }

        async fn merge_items(&self, todos: Vec<TodoItem>) -> Result<Vec<TodoItem>, ToolError> {
            let deduped = dedupe_by_id(todos);
            let mut items = self.items.lock().unwrap();
            let mut id_to_idx: HashMap<String, usize> = items
                .iter()
                .enumerate()
                .map(|(i, item)| (item.id.clone(), i))
                .collect();

            for t in &deduped {
                let id = t.id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                if let Some(&idx) = id_to_idx.get(&id) {
                    // Update existing: only apply non-empty content and valid status
                    if !t.content.trim().is_empty() {
                        items[idx].content = t.content.trim().to_string();
                    }
                    let status = t.status.trim().to_lowercase();
                    if VALID_STATUSES.contains(&status.as_str()) {
                        items[idx].status = status;
                    }
                } else {
                    // New item — validate and append
                    let validated = validate_item(t.clone());
                    id_to_idx.insert(validated.id.clone(), items.len());
                    items.push(validated);
                }
            }
            Ok(items.clone())
        }
    }

    fn item(id: &str, content: &str, status: &str) -> Value {
        json!({"id": id, "content": content, "status": status})
    }

    fn handler() -> TodoHandler {
        TodoHandler::new(Arc::new(MockTodoBackend::new()))
    }

    async fn exec(h: &TodoHandler, p: Value) -> Value {
        let s = h.execute(p).await.unwrap();
        serde_json::from_str(&s).unwrap()
    }

    // -----------------------------------------------------------------------
    // 1. Read mode returns todos + summary
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_read_returns_todos_and_summary() {
        let h = handler();
        exec(
            &h,
            json!({"todos": [item("a1", "Task one", "pending"), item("a2", "Task two", "completed")]}),
        )
        .await;

        let r = exec(&h, json!({})).await;
        assert_eq!(r["todos"].as_array().unwrap().len(), 2);
        assert_eq!(r["summary"]["total"], 2);
        assert_eq!(r["summary"]["pending"], 1);
        assert_eq!(r["summary"]["completed"], 1);
        assert_eq!(r["summary"]["in_progress"], 0);
    }

    // -----------------------------------------------------------------------
    // 2. Replace mode (merge=false) overwrites entire list
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_replace_mode() {
        let h = handler();
        exec(&h, json!({"todos": [item("old", "Old task", "pending")]})).await;

        let r = exec(
            &h,
            json!({"todos": [item("y1", "New task", "in_progress"), item("y2", "Another", "pending")]}),
        )
        .await;

        let todos = r["todos"].as_array().unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0]["id"], "y1");
        assert_eq!(todos[1]["id"], "y2");
    }

    // -----------------------------------------------------------------------
    // 3. Merge updates an existing item
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_merge_updates_existing() {
        let h = handler();
        exec(&h, json!({"todos": [item("m1", "Original", "pending")]})).await;

        let r = exec(
            &h,
            json!({
                "todos": [{"id": "m1", "content": "Original", "status": "in_progress"}],
                "merge": true
            }),
        )
        .await;

        let todos = r["todos"].as_array().unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0]["id"], "m1");
        assert_eq!(todos[0]["status"], "in_progress");
    }

    // -----------------------------------------------------------------------
    // 4. Merge appends new items while preserving existing ones
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_merge_appends_new_items() {
        let h = handler();
        exec(&h, json!({"todos": [item("e1", "Existing", "pending")]})).await;

        let r = exec(
            &h,
            json!({
                "todos": [item("n1", "New item", "pending")],
                "merge": true
            }),
        )
        .await;

        let todos = r["todos"].as_array().unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0]["id"], "e1");
        assert_eq!(todos[1]["id"], "n1");
    }

    // -----------------------------------------------------------------------
    // 5. Duplicate id dedup — keeps last occurrence at its sorted position
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_dedup_keeps_last_occurrence() {
        let h = handler();
        let r = exec(
            &h,
            json!({
                "todos": [
                    item("dup",   "First",  "pending"),
                    item("other", "Other",  "pending"),
                    item("dup",   "Last",   "in_progress"),
                ]
            }),
        )
        .await;

        let todos = r["todos"].as_array().unwrap();
        // last "dup" is at original index 2, "other" at 1 → sorted: [other(1), dup(2)]
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0]["id"], "other");
        assert_eq!(todos[1]["id"], "dup");
        assert_eq!(todos[1]["content"], "Last");
        assert_eq!(todos[1]["status"], "in_progress");
    }

    // -----------------------------------------------------------------------
    // 6. `cancelled` status is counted correctly in summary
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_cancelled_status_counted() {
        let h = handler();
        let r = exec(
            &h,
            json!({
                "todos": [
                    item("c1", "Task 1", "cancelled"),
                    item("c2", "Task 2", "cancelled"),
                    item("c3", "Task 3", "pending"),
                ]
            }),
        )
        .await;

        assert_eq!(r["summary"]["cancelled"], 2);
        assert_eq!(r["summary"]["pending"], 1);
        assert_eq!(r["summary"]["total"], 3);
        assert_eq!(r["summary"]["in_progress"], 0);
        assert_eq!(r["summary"]["completed"], 0);
    }

    // -----------------------------------------------------------------------
    // 7. Invalid status falls back to "pending"
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_invalid_status_falls_back_to_pending() {
        let h = handler();
        let r = exec(
            &h,
            json!({"todos": [{"id": "v1", "content": "Task", "status": "INVALID"}]}),
        )
        .await;

        assert_eq!(r["todos"][0]["status"], "pending");
    }

    // -----------------------------------------------------------------------
    // Extra: null todos → read mode
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_null_todos_is_read_mode() {
        let h = handler();
        let r = exec(&h, json!({"todos": null})).await;
        assert_eq!(r["summary"]["total"], 0);
        assert!(r["todos"].as_array().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // Extra: empty id and content get fallback values
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn test_empty_id_and_content_fallbacks() {
        let h = handler();
        let r = exec(
            &h,
            json!({"todos": [{"id": "", "content": "", "status": "pending"}]}),
        )
        .await;

        assert_eq!(r["todos"][0]["id"], "?");
        assert_eq!(r["todos"][0]["content"], "(no description)");
    }

    // -----------------------------------------------------------------------
    // Schema
    // -----------------------------------------------------------------------
    #[test]
    fn test_schema_name_is_todo() {
        assert_eq!(handler().schema().name, "todo");
    }

    #[test]
    fn test_schema_has_no_required_params() {
        // Both `todos` and `merge` are optional — required list should be empty.
        let schema = handler().schema();
        let required = schema.parameters.required.unwrap_or_default();
        assert!(required.is_empty());
    }
}
