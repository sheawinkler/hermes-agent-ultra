//! Docker terminal backend – executes commands inside Docker containers.

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

/// A [`TerminalBackend`] that runs commands inside a Docker container.
pub struct DockerBackend {
    /// Active container ID (or name).
    container_id: Option<String>,
    /// Docker image to use if creating a new container.
    image: Option<String>,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes.
    max_output_size: usize,
}

impl DockerBackend {
    /// Create a new Docker backend.
    ///
    /// - `container_id`: If Some, use an existing container. If None, one will
    ///   be created on first command execution using the specified `image`.
    /// - `image`: Docker image name (e.g. `"ubuntu:22.04"`). Used only when
    ///   `container_id` is None.
    pub fn new(
        container_id: Option<String>,
        image: Option<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        Self {
            container_id,
            image: image.or_else(|| Some("ubuntu:22.04".to_string())),
            default_timeout,
            max_output_size,
        }
    }

    /// Ensure we have a running container. If `container_id` is None,
    /// create one from the configured image.
    async fn ensure_container(&mut self) -> Result<String, AgentError> {
        if let Some(ref id) = self.container_id {
            return Ok(id.clone());
        }

        let image = self
            .image
            .as_ref()
            .ok_or_else(|| AgentError::Config("No Docker image specified".into()))?;

        tracing::info!("Creating Docker container from image: {}", image);

        let output = TokioCommand::new("docker")
            .args(["run", "-d", "--tty", image, "tail", "-f", "/dev/null"])
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to create Docker container: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AgentError::Io(format!(
                "Failed to create Docker container: {}",
                stderr
            )));
        }

        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        tracing::info!("Created container: {}", id);
        self.container_id = Some(id.clone());
        Ok(id)
    }

    /// Get the container ID, returning an error if not available.
    fn container_id(&self) -> Result<&str, AgentError> {
        self.container_id
            .as_deref()
            .ok_or_else(|| AgentError::Config("No Docker container ID available".into()))
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
impl TerminalBackend for DockerBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let container_id = self.container_id()?;
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        let mut args = vec!["exec".to_string()];

        if pty {
            args.push("-it".to_string());
        }

        if let Some(dir) = workdir {
            args.push("-w".to_string());
            args.push(dir.to_string());
        }

        args.push(container_id.to_string());
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        if background {
            // For background mode, we still run docker exec but detach.
            // Insert -d flag before the container id.
            args.insert(1, "-d".to_string());
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            async {
                let output = TokioCommand::new("docker")
                    .args(&args)
                    .output()
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to execute docker command: {}", e)))?;

                let stdout = self.truncate_output(String::from_utf8_lossy(&output.stdout).to_string());
                let stderr = self.truncate_output(String::from_utf8_lossy(&output.stderr).to_string());

                Ok(CommandOutput {
                    exit_code: output.status.code().unwrap_or(-1),
                    stdout,
                    stderr,
                })
            },
        )
        .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "Docker command timed out after {} seconds",
                timeout_secs
            ))),
        }
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        let container_id = self.container_id()?;

        // Use docker exec cat to read the file
        let mut cat_cmd = format!("cat {}", shlex_quote(path));
        if offset.is_some() || limit.is_some() {
            // Use sed for offset/limit support
            let start = offset.unwrap_or(0) + 1; // sed is 1-indexed
            if let Some(lim) = limit {
                cat_cmd = format!("sed -n '{},{}p' {}", start, start + lim - 1, shlex_quote(path));
            } else {
                cat_cmd = format!("sed -n '{},\\$p' {}", start, shlex_quote(path));
            }
        }

        let output = TokioCommand::new("docker")
            .args(["exec", container_id, "sh", "-c", &cat_cmd])
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to read file via docker: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AgentError::Io(format!(
                "Failed to read file '{}': {}",
                path, stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        let container_id = self.container_id()?;

        // Use docker exec to write the file. We pipe the content through stdin.
        // First ensure the parent directory exists.
        let parent_dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if !parent_dir.is_empty() {
            let mkdir_cmd = format!("mkdir -p {}", shlex_quote(&parent_dir));
            let mkdir_output = TokioCommand::new("docker")
                .args(["exec", container_id, "sh", "-c", &mkdir_cmd])
                .output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to create parent dir via docker: {}", e)))?;

            if !mkdir_output.status.success() {
                let stderr = String::from_utf8_lossy(&mkdir_output.stderr);
                return Err(AgentError::Io(format!(
                    "Failed to create parent directory '{}': {}",
                    parent_dir, stderr
                )));
            }
        }

        // Write content using docker exec with heredoc-style input
        // Escape any single quotes in the content
        let escaped_content = content.replace('\'', "'\\''");
        let write_cmd = format!("cat > {} << 'HERMES_EOF'\n{}\nHERMES_EOF", shlex_quote(path), escaped_content);

        let output = TokioCommand::new("docker")
            .args(["exec", container_id, "sh", "-c", &write_cmd])
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to write file via docker: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AgentError::Io(format!(
                "Failed to write file '{}': {}",
                path, stderr
            )));
        }

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let container_id = self.container_id()?;

        let output = TokioCommand::new("docker")
            .args([
                "exec",
                container_id,
                "test",
                "-e",
                path,
            ])
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to check file via docker: {}", e)))?;

        // test -e returns 0 if file exists, 1 otherwise
        Ok(output.status.success())
    }
}

/// Simple shell-style quoting for file paths.
fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        "''".to_string()
    } else if !s.chars().any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == '\\' || c == '$' || c == '`') {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shlex_quote() {
        assert_eq!(shlex_quote("simple"), "simple");
        assert_eq!(shlex_quote(""), "''");
        assert_eq!(shlex_quote("/path/with spaces"), "'/path/with spaces'");
        assert_eq!(shlex_quote("/path/with'quote"), "'/path/with'\\''quote'");
    }
}