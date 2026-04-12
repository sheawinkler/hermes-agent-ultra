//! Singularity terminal backend – executes commands inside Singularity/Apptainer containers.

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

/// A [`TerminalBackend`] that runs commands inside a Singularity (Apptainer) container.
///
/// This backend is well-suited for HPC environments where Docker is not
/// available but Singularity/Apptainer is the container runtime of choice.
pub struct SingularityBackend {
    /// Path to the Singularity image file (.sif, .simg) or a URI (e.g. docker://ubuntu).
    image_path: Option<String>,
    /// Path to the singularity binary (default: "singularity" or "apptainer").
    binary_path: Option<String>,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes.
    max_output_size: usize,
    /// Whether to use the Apptainer compatibility layer.
    use_apptainer: bool,
    /// Additional bind mount paths (host:container pairs).
    bind_paths: Vec<String>,
}

impl SingularityBackend {
    /// Create a new Singularity backend.
    ///
    /// - `image_path`: If None, falls back to the `SINGULARITY_IMAGE` env var.
    /// - `binary_path`: If None, auto-detects "apptainer" or "singularity".
    pub fn new(
        image_path: Option<String>,
        binary_path: Option<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        let image_path = image_path.or_else(|| std::env::var("SINGULARITY_IMAGE").ok());

        // Auto-detect the binary
        let binary_path = binary_path.or_else(|| {
            // Prefer apptainer if available (newer name for Singularity)
            std::env::var("SINGULARITY_BINARY").ok()
        });

        let use_apptainer = binary_path
            .as_deref()
            .map(|p| p.contains("apptainer"))
            .unwrap_or(false);

        Self {
            image_path,
            binary_path,
            default_timeout,
            max_output_size,
            use_apptainer,
            bind_paths: Vec::new(),
        }
    }

    /// Add a bind mount path (host_path:container_path).
    pub fn with_bind(mut self, bind: &str) -> Self {
        self.bind_paths.push(bind.to_string());
        self
    }

    /// Get the Singularity/Apptainer binary name.
    fn binary(&self) -> &str {
        self.binary_path
            .as_deref()
            .unwrap_or(if self.use_apptainer {
                "apptainer"
            } else {
                "singularity"
            })
    }

    /// Get the image path, returning an error if not configured.
    fn image_path(&self) -> Result<&str, AgentError> {
        self.image_path
            .as_deref()
            .ok_or_else(|| AgentError::Config("Singularity image path not configured".into()))
    }

    fn truncate_output(&self, s: String) -> String {
        if s.len() > self.max_output_size {
            s[..self.max_output_size].to_string()
        } else {
            s
        }
    }

    /// Build the common singularity exec arguments.
    fn build_exec_args(&self, workdir: Option<&str>) -> Vec<String> {
        let mut args = vec!["exec".to_string()];

        // Add bind mounts
        for bind in &self.bind_paths {
            args.push("--bind".to_string());
            args.push(bind.clone());
        }

        // Add working directory
        if let Some(dir) = workdir {
            args.push("--pwd".to_string());
            args.push(dir.to_string());
        }

        // Add the image
        if let Ok(image) = self.image_path() {
            args.push(image.to_string());
        }

        args
    }
}

#[async_trait]
impl TerminalBackend for SingularityBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let image = self.image_path()?;
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        let mut args = vec!["exec".to_string()];

        // Add bind mounts
        for bind in &self.bind_paths {
            args.push("--bind".to_string());
            args.push(bind.clone());
        }

        // Add working directory
        if let Some(dir) = workdir {
            args.push("--pwd".to_string());
            args.push(dir.to_string());
        }

        // Add the image
        args.push(image.to_string());

        // Add the command to execute
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        let binary = self.binary().to_string();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            async {
                let mut cmd = TokioCommand::new(&binary);
                cmd.args(&args);

                if background {
                    cmd.stdin(std::process::Stdio::null());
                }

                let output = cmd
                    .output()
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to execute singularity command: {}", e)))?;

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
                "Singularity command timed out after {} seconds",
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
        let image = self.image_path()?;

        // Build a cat/sed command inside the container
        let inner_cmd = if offset.is_some() || limit.is_some() {
            let start = offset.unwrap_or(0) + 1; // sed is 1-indexed
            if let Some(lim) = limit {
                format!("sed -n '{},{}p' {}", start, start + lim - 1, shlex_quote_singularity(path))
            } else {
                format!("sed -n '{},\\$p' {}", start, shlex_quote_singularity(path))
            }
        } else {
            format!("cat {}", shlex_quote_singularity(path))
        };

        let mut args = vec!["exec".to_string()];

        for bind in &self.bind_paths {
            args.push("--bind".to_string());
            args.push(bind.clone());
        }

        args.push(image.to_string());
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(inner_cmd);

        let binary = self.binary().to_string();
        let timeout_secs = self.default_timeout;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            async {
                let output = TokioCommand::new(&binary)
                    .args(&args)
                    .output()
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to read file via singularity: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(AgentError::Io(format!(
                        "Failed to read file '{}': {}",
                        path, stderr
                    )));
                }

                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            },
        )
        .await;

        match result {
            Ok(Ok(content)) => Ok(content),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "Singularity read_file timed out after {} seconds",
                timeout_secs
            ))),
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        let image = self.image_path()?;

        // Ensure parent directory exists inside the container
        let parent_dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if !parent_dir.is_empty() {
            let mkdir_cmd = format!("mkdir -p {}", shlex_quote_singularity(&parent_dir));
            let mut mkdir_args = vec!["exec".to_string()];
            for bind in &self.bind_paths {
                mkdir_args.push("--bind".to_string());
                mkdir_args.push(bind.clone());
            }
            mkdir_args.push(image.to_string());
            mkdir_args.push("sh".to_string());
            mkdir_args.push("-c".to_string());
            mkdir_args.push(mkdir_cmd);

            let binary = self.binary().to_string();
            let mkdir_output = TokioCommand::new(&binary)
                .args(&mkdir_args)
                .output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to create parent dir via singularity: {}", e)))?;

            if !mkdir_output.status.success() {
                let stderr = String::from_utf8_lossy(&mkdir_output.stderr);
                return Err(AgentError::Io(format!(
                    "Failed to create parent directory '{}': {}",
                    parent_dir, stderr
                )));
            }
        }

        // Write content using heredoc inside the container
        let escaped_content = content.replace('\'', "'\\''");
        let write_cmd = format!(
            "cat > {} << 'HERMES_EOF'\n{}\nHERMES_EOF",
            shlex_quote_singularity(path),
            escaped_content
        );

        let mut args = vec!["exec".to_string()];
        for bind in &self.bind_paths {
            args.push("--bind".to_string());
            args.push(bind.clone());
        }
        args.push(image.to_string());
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(write_cmd);

        let binary = self.binary().to_string();
        let output = TokioCommand::new(&binary)
            .args(&args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to write file via singularity: {}", e)))?;

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
        let image = self.image_path()?;

        let inner_cmd = format!("test -e {}", shlex_quote_singularity(path));

        let mut args = vec!["exec".to_string()];
        for bind in &self.bind_paths {
            args.push("--bind".to_string());
            args.push(bind.clone());
        }
        args.push(image.to_string());
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(inner_cmd);

        let binary = self.binary().to_string();
        let output = TokioCommand::new(&binary)
            .args(&args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to check file via singularity: {}", e)))?;

        Ok(output.status.success())
    }
}

/// Simple shell quoting for Singularity paths.
fn shlex_quote_singularity(s: &str) -> String {
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
    fn test_singularity_backend_default() {
        let backend = SingularityBackend::new(
            Some("ubuntu.sif".to_string()),
            None,
            120,
            1_048_576,
        );
        assert_eq!(backend.image_path().unwrap(), "ubuntu.sif");
    }

    #[test]
    fn test_singularity_backend_with_binds() {
        let backend = SingularityBackend::new(
            Some("ubuntu.sif".to_string()),
            Some("apptainer".to_string()),
            120,
            1_048_576,
        )
        .with_bind("/data:/data")
        .with_bind("/home:/home");

        assert!(backend.use_apptainer);
        assert_eq!(backend.binary(), "apptainer");
        assert_eq!(backend.bind_paths.len(), 2);
    }

    #[test]
    fn test_shlex_quote_singularity() {
        assert_eq!(shlex_quote_singularity("simple"), "simple");
        assert_eq!(shlex_quote_singularity(""), "''");
        assert_eq!(shlex_quote_singularity("/path/with spaces"), "'/path/with spaces'");
    }
}