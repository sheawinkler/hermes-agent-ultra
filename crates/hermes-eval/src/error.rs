//! Error types for the evaluation harness.

use thiserror::Error;

pub type EvalResult<T> = std::result::Result<T, EvalError>;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("dataset load failed: {0}")]
    DatasetLoad(String),

    #[error("task execution failed: {0}")]
    TaskExecution(String),

    #[error("verification failed: {0}")]
    Verification(String),

    #[error("timeout after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("{0}")]
    Other(String),
}
