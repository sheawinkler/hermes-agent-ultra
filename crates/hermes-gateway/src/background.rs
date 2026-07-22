//! Background task management for the gateway.
//!
//! Supports /background and /btw commands that run agent tasks asynchronously.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
use uuid::Uuid;

const DEFAULT_MAX_RETAINED_TASKS: usize = 256;
const WORKER_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) struct BackgroundWorkerGuard {
    handles: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    id: String,
}

impl Drop for BackgroundWorkerGuard {
    fn drop(&mut self) {
        self.handles.lock().unwrap().remove(&self.id);
    }
}

/// Manages background tasks.
pub struct BackgroundTaskManager {
    tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
    handles: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    accepting: Arc<AtomicBool>,
    max_concurrent: usize,
    max_tasks: usize,
    resource_limits_enabled: bool,
}

impl BackgroundTaskManager {
    pub fn new(max_concurrent: usize) -> Self {
        Self::with_resource_limits(max_concurrent, false)
    }

    pub fn with_resource_limits(max_concurrent: usize, resource_limits_enabled: bool) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            handles: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
            max_tasks: DEFAULT_MAX_RETAINED_TASKS.max(max_concurrent),
            resource_limits_enabled,
            accepting: Arc::new(AtomicBool::new(true)),
        }
    }

    fn active_worker_count(&self, tasks: &HashMap<String, BackgroundTask>) -> usize {
        let handles = self.handles.lock().unwrap();
        handles.len()
            + tasks
                .values()
                .filter(|task| {
                    task.status == TaskStatus::Running && !handles.contains_key(&task.id)
                })
                .count()
    }

    pub(crate) fn worker_guard(&self, id: impl Into<String>) -> BackgroundWorkerGuard {
        BackgroundWorkerGuard {
            handles: self.handles.clone(),
            id: id.into(),
        }
    }

    fn prune_oldest_terminal(tasks: &mut HashMap<String, BackgroundTask>, max_tasks: usize) {
        while tasks.len() >= max_tasks {
            let Some(oldest_id) = tasks
                .values()
                .filter(|task| task.status != TaskStatus::Running)
                .min_by(|left, right| {
                    left.finished_at
                        .unwrap_or(left.created_at)
                        .cmp(&right.finished_at.unwrap_or(right.created_at))
                        .then_with(|| left.id.cmp(&right.id))
                })
                .map(|task| task.id.clone())
            else {
                break;
            };
            tasks.remove(&oldest_id);
        }
    }

    /// Submit a new background task.
    pub fn submit(&self, prompt: String) -> Result<String, String> {
        let mut tasks = self.tasks.lock().unwrap();
        if !self.accepting.load(Ordering::Acquire) {
            return Err("Background task manager is shutting down".to_string());
        }
        let active_count = self.active_worker_count(&tasks);
        if active_count >= self.max_concurrent {
            return Err(format!(
                "Maximum concurrent background tasks ({}) reached. Wait for a task to complete.",
                self.max_concurrent
            ));
        }
        if self.resource_limits_enabled {
            Self::prune_oldest_terminal(&mut tasks, self.max_tasks);
        }
        let id = Uuid::new_v4().to_string();
        tasks.insert(
            id.clone(),
            BackgroundTask {
                id: id.clone(),
                prompt,
                status: TaskStatus::Running,
                result: None,
                created_at: chrono::Utc::now(),
                finished_at: None,
            },
        );
        Ok(id)
    }

    /// Submit a background task with a caller-chosen id (e.g. Python-shaped `bg_…` / `btw_…`).
    pub fn submit_with_id(&self, id: String, prompt: String) -> Result<String, String> {
        let mut tasks = self.tasks.lock().unwrap();
        if !self.accepting.load(Ordering::Acquire) {
            return Err("Background task manager is shutting down".to_string());
        }
        let active_count = self.active_worker_count(&tasks);
        if active_count >= self.max_concurrent {
            return Err(format!(
                "Maximum concurrent background tasks ({}) reached. Wait for a task to complete.",
                self.max_concurrent
            ));
        }
        if tasks.contains_key(&id) {
            return Err("Task id already exists".to_string());
        }
        if self.resource_limits_enabled {
            Self::prune_oldest_terminal(&mut tasks, self.max_tasks);
        }
        tasks.insert(
            id.clone(),
            BackgroundTask {
                id: id.clone(),
                prompt,
                status: TaskStatus::Running,
                result: None,
                created_at: chrono::Utc::now(),
                finished_at: None,
            },
        );
        Ok(id)
    }

    /// Mark a task as completed with a result.
    pub fn complete(&self, id: &str, result: String) -> bool {
        let mut tasks = self.tasks.lock().unwrap();
        let Some(task) = tasks.get_mut(id) else {
            return false;
        };
        if task.status != TaskStatus::Running {
            return false;
        }
        task.status = TaskStatus::Completed;
        task.result = Some(result);
        task.finished_at = Some(chrono::Utc::now());
        true
    }

    /// Mark a task as failed.
    pub fn fail(&self, id: &str, error: String) -> bool {
        let mut tasks = self.tasks.lock().unwrap();
        let Some(task) = tasks.get_mut(id) else {
            return false;
        };
        if task.status != TaskStatus::Running {
            return false;
        }
        task.status = TaskStatus::Failed(error);
        task.finished_at = Some(chrono::Utc::now());
        true
    }

    /// Attach the spawned worker to a task while it is still running.
    pub fn register_handle(&self, id: &str, handle: JoinHandle<()>) -> bool {
        let tasks = self.tasks.lock().unwrap();
        if !matches!(
            tasks.get(id).map(|task| &task.status),
            Some(TaskStatus::Running)
        ) {
            handle.abort();
            return false;
        }
        let mut handles = self.handles.lock().unwrap();
        if handles.contains_key(id) {
            handle.abort();
            return false;
        }
        handles.insert(id.to_string(), handle);
        true
    }

    /// Cancel a running task.
    pub fn cancel(&self, id: &str) -> bool {
        let mut tasks = self.tasks.lock().unwrap();
        let Some(task) = tasks.get_mut(id) else {
            return false;
        };
        if task.status != TaskStatus::Running {
            return false;
        }
        task.status = TaskStatus::Cancelled;
        task.finished_at = Some(chrono::Utc::now());
        let handle = self.handles.lock().unwrap().remove(id);
        drop(tasks);
        if let Some(handle) = handle {
            handle.abort();
        }
        true
    }

    /// Reopen admission after the gateway has started successfully.
    pub fn reopen(&self) {
        let _tasks = self.tasks.lock().unwrap();
        self.accepting.store(true, Ordering::Release);
    }

    /// Abort every registered worker, await teardown, and cancel admitted running tasks.
    pub async fn cancel_all(&self) -> usize {
        let mut tasks = self.tasks.lock().unwrap();
        self.accepting.store(false, Ordering::Release);
        let now = chrono::Utc::now();
        for task in tasks.values_mut() {
            if task.status == TaskStatus::Running {
                task.status = TaskStatus::Cancelled;
                task.finished_at = Some(now);
            }
        }
        let handles = std::mem::take(&mut *self.handles.lock().unwrap());
        drop(tasks);
        let aborted = handles.len();
        for handle in handles.values() {
            handle.abort();
        }
        let _ = tokio::time::timeout(WORKER_SHUTDOWN_TIMEOUT, async move {
            for handle in handles.into_values() {
                let _ = handle.await;
            }
        })
        .await;
        aborted
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
        tasks.retain(|_, task| {
            task.status == TaskStatus::Running
                || task.finished_at.unwrap_or(task.created_at) > cutoff
        });
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
        assert!(!mgr.complete(&id, "late".to_string()));
        assert!(!mgr.fail(&id, "late".to_string()));
        assert_eq!(mgr.get_status(&id), Some(TaskStatus::Cancelled));
        assert_eq!(mgr.get_result(&id), None);
    }

    #[tokio::test]
    async fn cancel_aborts_registered_worker() {
        struct DropSignal(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let manager = BackgroundTaskManager::new(1);
        let id = manager.submit("never finishes".to_string()).unwrap();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_worker = dropped.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _drop_signal = DropSignal(dropped_for_worker);
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
        });
        assert!(manager.register_handle(&id, handle));
        entered_rx.await.expect("worker should start");

        assert!(manager.cancel(&id));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancel should abort and drop the worker");
        assert_eq!(manager.get_status(&id), Some(TaskStatus::Cancelled));
        assert!(!manager.complete(&id, "late completion".to_string()));
    }

    #[tokio::test]
    async fn cancel_all_aborts_terminal_worker_before_notification_finishes() {
        struct DropSignal(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let manager = BackgroundTaskManager::new(1);
        let id = manager.submit("finishing".to_string()).unwrap();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_worker = dropped.clone();
        let manager_for_worker = Arc::new(manager);
        let worker_manager = manager_for_worker.clone();
        let worker_id = id.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _worker_guard = worker_manager.worker_guard(worker_id);
            let _drop_signal = DropSignal(dropped_for_worker);
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
        });
        assert!(manager_for_worker.register_handle(&id, handle));
        entered_rx.await.expect("worker should start");
        assert!(manager_for_worker.complete(&id, "done".to_string()));
        assert!(manager_for_worker.submit("must wait".to_string()).is_err());

        assert_eq!(manager_for_worker.cancel_all().await, 1);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown should abort terminal notification worker");
        assert_eq!(
            manager_for_worker.get_status(&id),
            Some(TaskStatus::Completed)
        );
    }

    #[test]
    fn test_list_tasks() {
        let mgr = BackgroundTaskManager::new(5);
        mgr.submit("task 1".to_string()).unwrap();
        mgr.submit("task 2".to_string()).unwrap();
        assert_eq!(mgr.list_tasks().len(), 2);
    }

    #[test]
    fn terminal_task_history_is_bounded() {
        let manager = BackgroundTaskManager::with_resource_limits(1, true);
        for index in 0..(DEFAULT_MAX_RETAINED_TASKS + 32) {
            let id = manager.submit(format!("task {index}")).unwrap();
            assert!(manager.complete(&id, format!("result {index}")));
        }

        let tasks = manager.list_tasks();
        assert_eq!(tasks.len(), DEFAULT_MAX_RETAINED_TASKS);
        assert!(tasks
            .iter()
            .all(|(_, status, _)| *status == TaskStatus::Completed));
    }
    #[test]
    fn terminal_task_history_is_unbounded_by_default() {
        let manager = BackgroundTaskManager::new(1);
        for index in 0..(DEFAULT_MAX_RETAINED_TASKS + 32) {
            let id = manager.submit(format!("task {index}")).unwrap();
            assert!(manager.complete(&id, format!("result {index}")));
        }

        assert_eq!(manager.list_tasks().len(), DEFAULT_MAX_RETAINED_TASKS + 32);
    }
}
