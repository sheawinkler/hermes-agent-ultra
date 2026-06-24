//! Shared stdin prompt helpers for CLI and gateway setup wizards.

use hermes_core::AgentError;

/// Read one line from stdin (blocking I/O on a worker thread).
pub async fn prompt_line(prompt: impl Into<String>) -> Result<String, AgentError> {
    let prompt = prompt.into();
    let line = tokio::task::spawn_blocking(move || {
        use std::io::{self, Write};
        print!("{}", prompt);
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("stdin task: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    Ok(line.trim().to_string())
}
