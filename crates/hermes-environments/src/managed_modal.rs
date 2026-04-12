//! Managed Modal environment that handles workspace lifecycle.
//!
//! Unlike the basic `ModalBackend`, this variant manages the full lifecycle
//! of a Modal workspace: creation, health-checking, and teardown.

use async_trait::async_trait;
use reqwest::Client;

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

/// A managed Modal backend that creates and destroys workspaces on demand.
pub struct ManagedModalBackend {
    api_key: String,
    workspace_id: Option<String>,
    gpu_type: Option<String>,
    default_timeout: u64,
    max_output_size: usize,
    client: Client,
}

impl ManagedModalBackend {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            workspace_id: None,
            gpu_type: None,
            default_timeout: 300,
            max_output_size: 1_048_576,
            client: Client::new(),
        }
    }

    /// Set the GPU type for newly created workspaces.
    pub fn with_gpu(mut self, gpu_type: &str) -> Self {
        self.gpu_type = Some(gpu_type.to_string());
        self
    }

    /// Set the default command timeout.
    pub fn with_timeout(mut self, timeout: u64) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Ensure a workspace exists, creating one if needed. Returns the workspace ID.
    pub async fn ensure_workspace(&mut self) -> Result<String, AgentError> {
        if let Some(ref id) = self.workspace_id {
            return Ok(id.clone());
        }

        let mut body = serde_json::json!({
            "name": format!("hermes-managed-{}", timestamp_id()),
        });
        if let Some(ref gpu) = self.gpu_type {
            body["gpu"] = serde_json::json!(gpu);
        }

        let resp = self
            .client
            .post("https://api.modal.com/v1/workspaces")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to create Modal workspace: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Modal workspace creation returned {}: {}",
                status, text
            )));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to parse workspace response: {}", e)))?;

        let id = data["id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();

        self.workspace_id = Some(id.clone());
        tracing::info!("Created managed Modal workspace: {}", id);
        Ok(id)
    }

    /// Destroy the current workspace and release resources.
    pub async fn destroy_workspace(&mut self) -> Result<(), AgentError> {
        let id = match self.workspace_id.take() {
            Some(id) => id,
            None => return Ok(()),
        };

        let url = format!("https://api.modal.com/v1/workspaces/{}", id);
        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to destroy workspace: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!(
                "Modal workspace destruction returned {}: {}",
                status,
                text
            );
        } else {
            tracing::info!("Destroyed managed Modal workspace: {}", id);
        }

        Ok(())
    }

    /// Get the current workspace ID, if any.
    pub fn workspace_id(&self) -> Option<&str> {
        self.workspace_id.as_deref()
    }

    fn truncate_output(&self, s: String) -> String {
        if s.len() > self.max_output_size {
            s[..self.max_output_size].to_string()
        } else {
            s
        }
    }
}

#[async_trait]
impl TerminalBackend for ManagedModalBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        _background: bool,
        _pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let ws_id = self.workspace_id.as_deref().ok_or_else(|| {
            AgentError::Io("No workspace active. Call ensure_workspace() first.".into())
        })?;

        let timeout_secs = timeout.unwrap_or(self.default_timeout);
        let body = serde_json::json!({
            "workspace_id": ws_id,
            "command": command,
            "workdir": workdir,
            "timeout": timeout_secs,
        });

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs + 10),
            self.client
                .post("https://api.modal.com/v1/execute")
                .bearer_auth(&self.api_key)
                .json(&body)
                .send(),
        )
        .await
        .map_err(|_| AgentError::Timeout("Managed Modal command timed out".into()))?
        .map_err(|e| AgentError::Io(format!("Modal execute failed: {}", e)))?;

        if !result.status().is_success() {
            let status = result.status();
            let text = result.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Modal execute returned {}: {}",
                status, text
            )));
        }

        let data: serde_json::Value = result
            .json()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to parse execute response: {}", e)))?;

        Ok(CommandOutput {
            exit_code: data["exit_code"].as_i64().unwrap_or(-1) as i32,
            stdout: self.truncate_output(
                data["stdout"].as_str().unwrap_or("").to_string(),
            ),
            stderr: self.truncate_output(
                data["stderr"].as_str().unwrap_or("").to_string(),
            ),
        })
    }

    async fn read_file(
        &self,
        path: &str,
        _offset: Option<u64>,
        _limit: Option<u64>,
    ) -> Result<String, AgentError> {
        if self.workspace_id.is_none() {
            return Err(AgentError::Io("No workspace active".into()));
        }
        let output = self
            .execute_command(&format!("cat {}", shell_escape(path)), None, None, false, false)
            .await?;
        if output.exit_code != 0 {
            return Err(AgentError::Io(format!(
                "read_file failed (exit {}): {}",
                output.exit_code, output.stderr
            )));
        }
        Ok(output.stdout)
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        if self.workspace_id.is_none() {
            return Err(AgentError::Io("No workspace active".into()));
        }
        let escaped = content.replace('\'', "'\\''");
        let cmd = format!("printf '%s' '{}' > {}", escaped, shell_escape(path));
        let output = self
            .execute_command(&cmd, None, None, false, false)
            .await?;
        if output.exit_code != 0 {
            return Err(AgentError::Io(format!(
                "write_file failed (exit {}): {}",
                output.exit_code, output.stderr
            )));
        }
        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let output = self
            .execute_command(
                &format!("test -e {} && echo yes || echo no", shell_escape(path)),
                None,
                None,
                false,
                false,
            )
            .await?;
        Ok(output.stdout.trim() == "yes")
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn timestamp_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:016x}", nanos)
}
