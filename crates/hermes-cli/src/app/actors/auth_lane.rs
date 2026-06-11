//! Serializes runtime OAuth/API-key refresh so concurrent callers do not stampede.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::Mutex;

/// Ensures at most one auth refresh runs at a time per interactive session.
#[derive(Clone, Default)]
pub struct AuthLane {
    gate: Arc<Mutex<()>>,
}

impl AuthLane {
    pub fn new() -> Self {
        Self {
            gate: Arc::new(Mutex::new(())),
        }
    }

    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.gate.lock().await
    }

    pub async fn run_serial<F, T>(&self, operation: F) -> T
    where
        F: Future<Output = T>,
    {
        let _guard = self.gate.lock().await;
        operation.await
    }
}
