//! Docker terminal backend – executes commands inside Docker containers.

use std::sync::Mutex;

use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

/// A [`TerminalBackend`] that runs commands inside a Docker container.
pub struct DockerBackend {
    /// Active container ID (or name).
    container_id: Mutex<Option<String>>,
    /// Docker image to use if creating a new container.
    image: Option<String>,
    /// Mount the host cwd at /workspace when creating a container.
    mount_cwd_to_workspace: bool,
    /// Run the container as the host uid/gid when supported.
    run_as_host_user: bool,
    /// CPU limit for newly-created containers.
    container_cpu: Option<u32>,
    /// Memory limit in MiB for newly-created containers.
    container_memory: Option<u64>,
    /// Disk limit in MiB for newly-created containers.
    container_disk: Option<u64>,
    /// Keep container after it stops.
    container_persistent: bool,
    /// Extra env vars to pass to Docker as KEY=VALUE entries.
    docker_env: Option<String>,
    /// Host env-var names to forward.
    docker_forward_env: Vec<String>,
    /// Extra Docker volume specs.
    docker_volumes: Vec<String>,
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
        mount_cwd_to_workspace: bool,
        run_as_host_user: bool,
        container_cpu: Option<u32>,
        container_memory: Option<u64>,
        container_disk: Option<u64>,
        container_persistent: bool,
        docker_env: Option<String>,
        docker_forward_env: Vec<String>,
        docker_volumes: Vec<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        Self {
            container_id: Mutex::new(container_id),
            image: image.or_else(|| Some("ubuntu:22.04".to_string())),
            mount_cwd_to_workspace,
            run_as_host_user,
            container_cpu,
            container_memory,
            container_disk,
            container_persistent,
            docker_env,
            docker_forward_env,
            docker_volumes,
            default_timeout,
            max_output_size,
        }
    }

    fn docker_env_pairs(&self) -> Vec<String> {
        self.docker_env
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty() && entry.contains('='))
            .map(ToString::to_string)
            .collect()
    }

    fn build_run_args(&self, image: &str) -> Vec<String> {
        let mut args = vec!["run".to_string(), "-d".to_string(), "--tty".to_string()];
        if !self.container_persistent {
            args.push("--rm".to_string());
        }
        if self.run_as_host_user {
            #[cfg(unix)]
            {
                let uid = unsafe { libc::getuid() };
                let gid = unsafe { libc::getgid() };
                args.push("--user".to_string());
                args.push(format!("{uid}:{gid}"));
            }
        }
        if let Some(cpu) = self.container_cpu {
            args.push("--cpus".to_string());
            args.push(cpu.to_string());
        }
        if let Some(memory) = self.container_memory {
            args.push("--memory".to_string());
            args.push(format!("{memory}m"));
        }
        if let Some(disk) = self.container_disk {
            args.push("--storage-opt".to_string());
            args.push(format!("size={disk}m"));
        }
        for env_pair in self.docker_env_pairs() {
            args.push("-e".to_string());
            args.push(env_pair);
        }
        for env_name in &self.docker_forward_env {
            if let Ok(value) = std::env::var(env_name) {
                args.push("-e".to_string());
                args.push(format!("{env_name}={value}"));
            }
        }
        for volume in &self.docker_volumes {
            args.push("-v".to_string());
            args.push(volume.clone());
        }
        if self.mount_cwd_to_workspace {
            if let Ok(cwd) = std::env::current_dir() {
                args.push("-v".to_string());
                args.push(format!("{}:/workspace", cwd.display()));
                args.push("-w".to_string());
                args.push("/workspace".to_string());
            }
        }
        args.push(image.to_string());
        args.push("tail".to_string());
        args.push("-f".to_string());
        args.push("/dev/null".to_string());
        args
    }

    /// Ensure we have a running container. If `container_id` is None,
    /// create one from the configured image.
    async fn ensure_container(&self) -> Result<String, AgentError> {
        if let Some(id) = self
            .container_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Ok(id);
        }

        let image = self
            .image
            .as_ref()
            .ok_or_else(|| AgentError::Config("No Docker image specified".into()))?;

        tracing::info!("Creating Docker container from image: {}", image);

        let args = self.build_run_args(image);

        let output = TokioCommand::new("docker")
            .args(&args)
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
        *self.container_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
        Ok(id)
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
        let container_id = self.ensure_container().await?;
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

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
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
        })
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
        let container_id = self.ensure_container().await?;

        // Use docker exec cat to read the file
        let mut cat_cmd = format!("cat {}", shlex_quote(path));
        if offset.is_some() || limit.is_some() {
            // Use sed for offset/limit support
            let start = offset.unwrap_or(0) + 1; // sed is 1-indexed
            if let Some(lim) = limit {
                cat_cmd = format!(
                    "sed -n '{},{}p' {}",
                    start,
                    start + lim - 1,
                    shlex_quote(path)
                );
            } else {
                cat_cmd = format!("sed -n '{},\\$p' {}", start, shlex_quote(path));
            }
        }

        let output = TokioCommand::new("docker")
            .args(["exec", container_id.as_str(), "sh", "-c", &cat_cmd])
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
        let container_id = self.ensure_container().await?;

        // Use docker exec to write the file. We pipe the content through stdin.
        // First ensure the parent directory exists.
        let parent_dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if !parent_dir.is_empty() {
            let mkdir_cmd = format!("mkdir -p {}", shlex_quote(&parent_dir));
            let mkdir_output = TokioCommand::new("docker")
                .args(["exec", container_id.as_str(), "sh", "-c", &mkdir_cmd])
                .output()
                .await
                .map_err(|e| {
                    AgentError::Io(format!("Failed to create parent dir via docker: {}", e))
                })?;

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
        let write_cmd = format!(
            "cat > {} << 'HERMES_EOF'\n{}\nHERMES_EOF",
            shlex_quote(path),
            escaped_content
        );

        let output = TokioCommand::new("docker")
            .args(["exec", container_id.as_str(), "sh", "-c", &write_cmd])
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
        let container_id = self.ensure_container().await?;

        let output = TokioCommand::new("docker")
            .args(["exec", container_id.as_str(), "test", "-e", path])
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to check file via docker: {}", e)))?;

        // test -e returns 0 if file exists, 1 otherwise
        Ok(output.status.success())
    }
}

#[cfg(test)]
mod quote_tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn build_run_args_include_terminal_config_env_and_resource_flags() {
        let _forward = EnvGuard::set("HERMES_DOCKER_FORWARD_TEST", "forwarded");
        let backend = DockerBackend::new(
            None,
            Some("rust:1.90".to_string()),
            false,
            true,
            Some(2),
            Some(4096),
            Some(51200),
            true,
            Some("FOO=bar,BAZ=qux".to_string()),
            vec!["HERMES_DOCKER_FORWARD_TEST".to_string()],
            vec!["/host/cache:/cache".to_string()],
            120,
            1_048_576,
        );

        let args = backend.build_run_args("rust:1.90");
        assert!(args.windows(2).any(|w| w == ["--cpus", "2"]));
        assert!(args.windows(2).any(|w| w == ["--memory", "4096m"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--storage-opt", "size=51200m"]));
        assert!(args.windows(2).any(|w| w == ["-e", "FOO=bar"]));
        assert!(args.windows(2).any(|w| w == ["-e", "BAZ=qux"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["-e", "HERMES_DOCKER_FORWARD_TEST=forwarded"]));
        assert!(args.windows(2).any(|w| w == ["-v", "/host/cache:/cache"]));
        assert!(!args.iter().any(|arg| arg == "--rm"));
        #[cfg(unix)]
        assert!(args.iter().any(|arg| arg == "--user"));
    }
}

/// Simple shell-style quoting for file paths.
fn shlex_quote(s: &str) -> String {
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
    fn test_shlex_quote() {
        assert_eq!(shlex_quote("simple"), "simple");
        assert_eq!(shlex_quote(""), "''");
        assert_eq!(shlex_quote("/path/with spaces"), "'/path/with spaces'");
        assert_eq!(shlex_quote("/path/with'quote"), "'/path/with'\\''quote'");
    }
}
