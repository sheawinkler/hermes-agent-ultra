//! Background task management for the gateway.
//!
//! Supports /background and /btw commands that run agent tasks asynchronously.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Status of a background task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Completed,
    Failed(String),
    Cancelled,
}

/// A background task entry.
#[derive(Debug)]
pub struct BackgroundTask {
    pub id: String,
    pub prompt: String,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Manages background tasks.
pub struct BackgroundTaskManager {
    tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
    handles: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    max_concurrent: usize,
}

impl BackgroundTaskManager {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            handles: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
        }
    }

    /// Submit a new background task.
    pub fn submit(&self, prompt: String) -> Result<String, String> {
        let tasks = self.tasks.lock().unwrap();
        let running_count = tasks
            .values()
            .filter(|t| t.status == TaskStatus::Running)
            .count();
        if running_count >= self.max_concurrent {
            return Err(format!(
                "Maximum concurrent background tasks ({}) reached. Wait for a task to complete.",
                self.max_concurrent
            ));
        }
        drop(tasks);

        let id = Uuid::new_v4().to_string();
        let task = BackgroundTask {
            id: id.clone(),
            prompt,
            status: TaskStatus::Running,
            result: None,
            created_at: chrono::Utc::now(),
        };

        self.tasks.lock().unwrap().insert(id.clone(), task);
        Ok(id)
    }

    /// Mark a task as completed with a result.
    pub fn complete(&self, id: &str, result: String) {
        if let Some(task) = self.tasks.lock().unwrap().get_mut(id) {
            task.status = TaskStatus::Completed;
            task.result = Some(result);
        }
    }

    /// Mark a task as failed.
    pub fn fail(&self, id: &str, error: String) {
        if let Some(task) = self.tasks.lock().unwrap().get_mut(id) {
            task.status = TaskStatus::Failed(error);
        }
    }

    /// Cancel a running task.
    pub fn cancel(&self, id: &str) -> bool {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(task) = tasks.get_mut(id) {
            if task.status == TaskStatus::Running {
                task.status = TaskStatus::Cancelled;
                drop(tasks);
                if let Some(handle) = self.handles.lock().unwrap().remove(id) {
                    handle.abort();
                }
                return true;
            }
        }
        false
    }

    /// Get the status of a task.
    pub fn get_status(&self, id: &str) -> Option<TaskStatus> {
        self.tasks.lock().unwrap().get(id).map(|t| t.status.clone())
    }

    /// List all tasks.
    pub fn list_tasks(&self) -> Vec<(String, TaskStatus, String)> {
        self.tasks
            .lock()
            .unwrap()
            .values()
            .map(|t| (t.id.clone(), t.status.clone(), t.prompt.clone()))
            .collect()
    }

    /// Get a task's result.
    pub fn get_result(&self, id: &str) -> Option<String> {
        self.tasks
            .lock()
            .unwrap()
            .get(id)
            .and_then(|t| t.result.clone())
    }

    /// Clean up completed/failed/cancelled tasks older than the given duration.
    pub fn cleanup(&self, max_age: chrono::Duration) {
        let cutoff = chrono::Utc::now() - max_age;
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|_, t| t.status == TaskStatus::Running || t.created_at > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submit_and_complete() {
        let mgr = BackgroundTaskManager::new(5);
        let id = mgr.submit("test task".to_string()).unwrap();
        assert_eq!(mgr.get_status(&id), Some(TaskStatus::Running));
        mgr.complete(&id, "done".to_string());
        assert_eq!(mgr.get_status(&id), Some(TaskStatus::Completed));
        assert_eq!(mgr.get_result(&id), Some("done".to_string()));
    }

    #[test]
    fn test_max_concurrent() {
        let mgr = BackgroundTaskManager::new(1);
        let _id1 = mgr.submit("task 1".to_string()).unwrap();
        let result = mgr.submit("task 2".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_cancel() {
        let mgr = BackgroundTaskManager::new(5);
        let id = mgr.submit("task".to_string()).unwrap();
        assert!(mgr.cancel(&id));
        assert_eq!(mgr.get_status(&id), Some(TaskStatus::Cancelled));
    }

    #[test]
    fn test_list_tasks() {
        let mgr = BackgroundTaskManager::new(5);
        mgr.submit("task 1".to_string()).unwrap();
        mgr.submit("task 2".to_string()).unwrap();
        assert_eq!(mgr.list_tasks().len(), 2);
    }
}
