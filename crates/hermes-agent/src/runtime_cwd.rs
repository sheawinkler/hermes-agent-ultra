//! Task-local workspace context for a single agent invocation.
//!
//! Scheduled jobs can run concurrently with different working directories.
//! Process environment variables cannot represent that safely, so callers scope
//! the workspace path to the async task that owns the invocation.

use std::future::Future;
use std::path::PathBuf;

tokio::task_local! {
    static WORKSPACE_CWD: Option<PathBuf>;
}

/// Runs `future` with an invocation-local workspace directory.
pub async fn with_workspace_cwd<T>(workdir: Option<PathBuf>, future: impl Future<Output = T>) -> T {
    WORKSPACE_CWD.scope(workdir, future).await
}

/// Returns the workspace directory scoped to the current agent invocation.
pub fn workspace_cwd() -> Option<PathBuf> {
    WORKSPACE_CWD.try_with(Clone::clone).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_cwd_is_isolated_between_concurrent_tasks() {
        let first = tokio::spawn(with_workspace_cwd(
            Some(PathBuf::from("/workspace/first")),
            async {
                tokio::task::yield_now().await;
                workspace_cwd()
            },
        ));
        let second = tokio::spawn(with_workspace_cwd(
            Some(PathBuf::from("/workspace/second")),
            async {
                tokio::task::yield_now().await;
                workspace_cwd()
            },
        ));

        assert_eq!(first.await.expect("first task"), Some(PathBuf::from("/workspace/first")));
        assert_eq!(
            second.await.expect("second task"),
            Some(PathBuf::from("/workspace/second"))
        );
        assert_eq!(workspace_cwd(), None);
    }
}
