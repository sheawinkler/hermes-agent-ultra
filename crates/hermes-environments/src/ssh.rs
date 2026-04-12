//! SSH terminal backend – executes commands on a remote host via SSH.

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

/// A [`TerminalBackend`] that runs commands on a remote machine via SSH.
pub struct SshBackend {
    /// Remote hostname or IP address.
    host: String,
    /// SSH port (default: 22).
    port: u16,
    /// Username for SSH authentication.
    username: Option<String>,
    /// Path to the private key file.
    key_path: Option<String>,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes.
    max_output_size: usize,
}

impl SshBackend {
    /// Create a new SSH backend.
    pub fn new(
        host: String,
        port: u16,
        username: Option<String>,
        key_path: Option<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        Self {
            host,
            port,
            username,
            key_path,
            default_timeout,
            max_output_size,
        }
    }

    /// Build the base SSH command arguments (host, port, identity, etc.).
    fn build_ssh_args(&self) -> Vec<String> {
        let mut args = vec![
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(), // Non-interactive
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
        ];

        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }

        if let Some(ref key) = self.key_path {
            args.push("-i".to_string());
            args.push(key.clone());
        }

        // Build the destination string
        let destination = if let Some(ref user) = self.username {
            format!("{}@{}", user, self.host)
        } else {
            self.host.clone()
        };
        args.push(destination);

        args
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
impl TerminalBackend for SshBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        let mut ssh_args = self.build_ssh_args();

        // Build the remote command
        let mut remote_command = String::new();
        if let Some(dir) = workdir {
            remote_command.push_str(&format!("cd {} && ", shlex_quote_local(dir)));
        }

        if background {
            // Run in background on the remote machine using nohup
            remote_command.push_str(&format!("nohup {} &>/dev/null &", command));
        } else {
            remote_command.push_str(command);
        }

        ssh_args.push(remote_command);

        if pty {
            // Insert -t flag for PTY allocation
            ssh_args.insert(0, "-t".to_string());
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let output = TokioCommand::new("ssh")
                .args(&ssh_args)
                .output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to execute SSH command: {}", e)))?;

            let stdout = self.truncate_output(String::from_utf8_lossy(&output.stdout).to_string());
            let stderr = self.truncate_output(String::from_utf8_lossy(&output.stderr).to_string());

            Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout,
                stderr,
            })
        })
        .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "SSH command timed out after {} seconds",
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
        // Build a remote command to read the file with offset/limit
        let mut remote_cmd = String::new();

        if offset.is_some() || limit.is_some() {
            let start = offset.unwrap_or(0) + 1; // sed is 1-indexed
            if let Some(lim) = limit {
                remote_cmd = format!(
                    "sed -n '{},{}p' {}",
                    start,
                    start + lim - 1,
                    shlex_quote_local(path)
                );
            } else {
                remote_cmd = format!("sed -n '{},\\$p' {}", start, shlex_quote_local(path));
            }
        } else {
            remote_cmd = format!("cat {}", shlex_quote_local(path));
        }

        let mut ssh_args = self.build_ssh_args();
        ssh_args.push(remote_cmd);

        let timeout_secs = self.default_timeout;
        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let output = TokioCommand::new("ssh")
                .args(&ssh_args)
                .output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to read file via SSH: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(AgentError::Io(format!(
                    "Failed to read file '{}': {}",
                    path, stderr
                )));
            }

            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        })
        .await;

        match result {
            Ok(Ok(content)) => Ok(content),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "SSH read_file timed out after {} seconds",
                timeout_secs
            ))),
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        // Ensure parent directory exists
        let parent_dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if !parent_dir.is_empty() {
            let mkdir_cmd = format!("mkdir -p {}", shlex_quote_local(&parent_dir));
            let mut ssh_args = self.build_ssh_args();
            ssh_args.push(mkdir_cmd);

            let mkdir_output = TokioCommand::new("ssh")
                .args(&ssh_args)
                .output()
                .await
                .map_err(|e| {
                    AgentError::Io(format!("Failed to create parent dir via SSH: {}", e))
                })?;

            if !mkdir_output.status.success() {
                let stderr = String::from_utf8_lossy(&mkdir_output.stderr);
                return Err(AgentError::Io(format!(
                    "Failed to create parent directory '{}': {}",
                    parent_dir, stderr
                )));
            }
        }

        // Write content using heredoc over SSH
        let escaped_content = content.replace('\'', "'\\''");
        let write_cmd = format!(
            "cat > {} << 'HERMES_EOF'\n{}\nHERMES_EOF",
            shlex_quote_local(path),
            escaped_content
        );

        let mut ssh_args = self.build_ssh_args();
        ssh_args.push(write_cmd);

        let output = TokioCommand::new("ssh")
            .args(&ssh_args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to write file via SSH: {}", e)))?;

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
        let test_cmd = format!("test -e {}", shlex_quote_local(path));
        let mut ssh_args = self.build_ssh_args();
        ssh_args.push(test_cmd);

        let output = TokioCommand::new("ssh")
            .args(&ssh_args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to check file via SSH: {}", e)))?;

        Ok(output.status.success())
    }
}

/// Simple shell quoting for local arguments (paths, etc.).
fn shlex_quote_local(s: &str) -> String {
    if s.is_empty() {
        "''".to_string()
    } else if !s
        .chars()
        .any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == '\\' || c == '$' || c == '`')
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_args_construction() {
        let backend = SshBackend::new(
            "example.com".to_string(),
            22,
            Some("user".to_string()),
            Some("/home/user/.ssh/id_rsa".to_string()),
            120,
            1_048_576,
        );

        let args = backend.build_ssh_args();
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"/home/user/.ssh/id_rsa".to_string()));
        assert!(args.contains(&"user@example.com".to_string()));
    }

    #[test]
    fn test_ssh_args_custom_port() {
        let backend = SshBackend::new("example.com".to_string(), 2222, None, None, 120, 1_048_576);

        let args = backend.build_ssh_args();
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"2222".to_string()));
        assert!(args.contains(&"example.com".to_string()));
    }

    #[test]
    fn test_shlex_quote_local() {
        assert_eq!(shlex_quote_local("simple"), "simple");
        assert_eq!(shlex_quote_local(""), "''");
        assert_eq!(
            shlex_quote_local("/path/with spaces"),
            "'/path/with spaces'"
        );
    }
}
