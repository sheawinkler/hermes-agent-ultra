//! Docker terminal backend – executes commands inside least-privilege containers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};


use async_trait::async_trait;
use tokio::process::Command as TokioCommand;

use hermes_core::{
    subprocess::CommandNoWindowExt, subprocess_env::should_strip_subprocess_env_key, AgentError,
    CommandOutput, ResolvedExecutionMode, TerminalBackend,
};
use crate::containment::{
    ContainmentCommandRequest, ExecutionContainmentAdmissionError,
    ExecutionContainmentContext, HardenedCommandPlan, HardenedCommandPlanFailure,
    VerifiedIsolatedBackendCapability,
};


const DEFAULT_CONTAINER_CPUS: u32 = 1;
const DEFAULT_CONTAINER_MEMORY_MIB: u64 = 512;
const DEFAULT_CONTAINER_PIDS: u32 = 256;
const CONTAINER_TMPFS: &str = "/tmp:rw,nosuid,nodev,noexec,size=64m";

/// A typed opt-in for mounting the current host workspace read-write.
///
/// This is deliberately separate from configuration booleans: a string or
/// environment label must not be mistaken for a containment proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockerManualWorkspaceMountCapability {
    _private: (),
}

/// What this backend can honestly establish about a command's container.
///
/// The unverified variant is intentionally not an isolation certificate for
/// policy/approval: an externally supplied container can have arbitrary
/// mounts, privileges, and network access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerContainmentProof {
    LeastPrivilegeContainer,
    ExistingContainerUnverified,
}

/// Typed reason a Docker configuration cannot issue a closed-execution
/// containment certificate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DockerContainmentCertificationError {
    #[error("an externally supplied Docker container cannot be certified")]
    ExistingContainer,
    #[error("a Docker backend with a host workspace mount cannot be certified")]
    HostWorkspaceMount,
    #[error("a Docker backend running as the host user cannot be certified")]
    HostUser,
    #[error("a persistent Docker container cannot be certified")]
    PersistentContainer,
    #[error("a Docker backend with explicit container environment cannot be certified")]
    ExplicitContainerEnvironment,
    #[error("a Docker backend forwarding host environment cannot be certified")]
    HostEnvironmentForwarding,
    #[error("a Docker backend with host volumes cannot be certified")]
    HostVolume,
    #[error("a Docker image must be pinned by a sha256 digest before certification")]
    MutableImage,
    #[error("the supplied containment certificate does not match this Docker configuration")]
    CertificateConfigurationMismatch,
    #[error("manual mode must use an explicit host-local execution context")]
    ManualMode,
    #[error(transparent)]
    Admission(#[from] ExecutionContainmentAdmissionError),
}

/// A [`TerminalBackend`] that runs commands inside a Docker container.
pub struct DockerBackend {
    /// Active container ID (or name).
    container_id: Mutex<Option<String>>,
    /// Docker image to use if creating a new container.
    image: Option<String>,
    /// A requested host cwd mount; it only takes effect with the typed manual
    /// capability below.
    mount_cwd_to_workspace: bool,
    manual_workspace_mount: Option<DockerManualWorkspaceMountCapability>,
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
    /// Explicit non-credential env vars to pass as KEY=VALUE entries.
    docker_env: Option<String>,
    /// Retained for config compatibility; host env forwarding is refused.
    docker_forward_env: Vec<String>,
    /// Retained for config compatibility; arbitrary host volumes are refused.
    docker_volumes: Vec<String>,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes.
    max_output_size: usize,
    /// Only compiled for Docker unit tests. Production command execution is
    /// always the hardened Docker path below.
    #[cfg(test)]
    test_command_runner: Option<Arc<dyn DockerTestCommandRunner>>,
}

/// Test-only seam for exercising the exact hardened configuration without a
/// Docker daemon. It is deliberately absent from production builds.
#[cfg(test)]
#[async_trait]
trait DockerTestCommandRunner: Send + Sync {
    async fn create_container(&self, args: &[String]) -> Result<String, AgentError>;
    async fn execute_command(&self, args: &[String]) -> Result<CommandOutput, AgentError>;
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
            manual_workspace_mount: None,
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
            #[cfg(test)]
            test_command_runner: None,
        }
    }

    #[cfg(test)]
    fn with_test_command_runner(mut self, runner: Arc<dyn DockerTestCommandRunner>) -> Self {
        self.test_command_runner = Some(runner);
        self
    }


    /// Produce the typed capability needed for an intentional manual workspace
    /// mount. Callers must still pass canonical-path validation.
    pub fn manual_workspace_mount_capability() -> DockerManualWorkspaceMountCapability {
        DockerManualWorkspaceMountCapability { _private: () }
    }

    /// Enable a requested workspace mount only through a typed manual
    /// capability. This does not change the containment proof into a policy
    /// bypass: read-write host mounts remain host-sensitive.
    pub fn with_manual_workspace_mount(
        mut self,
        capability: DockerManualWorkspaceMountCapability,
    ) -> Self {
        self.manual_workspace_mount = Some(capability);
        self
    }

    /// Return a typed statement of what this backend can prove. Existing
    /// containers are never represented as least-privilege containers.
    pub fn containment_proof(&self) -> DockerContainmentProof {
        if self
            .container_id.lock()
            .is_some()
        {
            DockerContainmentProof::ExistingContainerUnverified
        } else {
            DockerContainmentProof::LeastPrivilegeContainer
        }
    }

    /// Issue the sole containment capability for this exact immutable Docker
    /// launch configuration.
    ///
    /// A "least privilege" backend label is not enough. Certification rejects
    /// every configuration that can expose host state and requires an
    /// immutable image digest before returning the opaque capability.
    pub fn verified_isolated_backend_capability(
        &self,
    ) -> Result<VerifiedIsolatedBackendCapability, DockerContainmentCertificationError> {
        let canonical = self.canonical_hardened_certification_config()?;
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        Ok(VerifiedIsolatedBackendCapability::issue_hardened_docker(
            hex::encode(hasher.finalize()),
        ))
    }

    /// Admit a closed execution mode against this exact hardened Docker
    /// configuration and bind the resulting certificate to this backend's
    /// actual Docker command plan.
    pub fn closed_execution_context(
        self: &Arc<Self>,
        mode: ResolvedExecutionMode,
    ) -> Result<ExecutionContainmentContext, DockerContainmentCertificationError> {
        let capability = self.verified_isolated_backend_capability()?;
        self.closed_execution_context_with_capability(mode, capability)
    }

    /// Bind a previously issued capability to this exact backend configuration
    /// and its actual hardened Docker command plan.
    ///
    /// This is intentionally the only public path from a capability to a
    /// closed-mode execution context, so a certificate from one Docker
    /// configuration cannot be paired with another backend or a host runner.
    pub fn closed_execution_context_with_capability(
        self: &Arc<Self>,
        mode: ResolvedExecutionMode,
        capability: VerifiedIsolatedBackendCapability,
    ) -> Result<ExecutionContainmentContext, DockerContainmentCertificationError> {
        if !mode.mode.requires_containment_certificate() {
            return Err(DockerContainmentCertificationError::ManualMode);
        }
        let expected = self.verified_isolated_backend_capability()?;
        if capability != expected {
            return Err(DockerContainmentCertificationError::CertificateConfigurationMismatch);
        }
        let plan: Arc<dyn HardenedCommandPlan> = Arc::new(DockerHardenedExecutionPlan {
            backend: Arc::clone(self),
        });
        Ok(ExecutionContainmentContext::certified(mode, capability, plan)?)
    }

    fn canonical_hardened_certification_config(
        &self,
    ) -> Result<String, DockerContainmentCertificationError> {
        if self.container_id.lock().is_some() {
            return Err(DockerContainmentCertificationError::ExistingContainer);
        }
        if self.mount_cwd_to_workspace || self.manual_workspace_mount.is_some() {
            return Err(DockerContainmentCertificationError::HostWorkspaceMount);
        }
        if self.run_as_host_user {
            return Err(DockerContainmentCertificationError::HostUser);
        }
        if self.container_persistent {
            return Err(DockerContainmentCertificationError::PersistentContainer);
        }
        if self
            .docker_env
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(DockerContainmentCertificationError::ExplicitContainerEnvironment);
        }
        if !self.docker_forward_env.is_empty() {
            return Err(DockerContainmentCertificationError::HostEnvironmentForwarding);
        }
        if !self.docker_volumes.is_empty() {
            return Err(DockerContainmentCertificationError::HostVolume);
        }

        let image = self
            .image
            .as_deref()
            .map(str::trim)
            .filter(|value| immutable_image_reference(value))
            .ok_or(DockerContainmentCertificationError::MutableImage)?;
        let cpu = self
            .container_cpu
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_CONTAINER_CPUS);
        let memory_mib = self
            .container_memory
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_CONTAINER_MEMORY_MIB);
        let disk_mib = self
            .container_disk
            .filter(|value| *value > 0)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string());

        // Every value in this canonical form is non-secret: nonempty explicit
        // environment values are rejected above, and this string is only
        // emitted through its SHA-256 digest.
        Ok(format!(
            concat!(
                "backend=docker\n",
                "scope=closed_execution\n",
                "policy_version=1\n",
                "launch_profile=docker_least_privilege_v1\n",
                "image={image}\n",
                "network=none\n",
                "cap_drop=ALL\n",
                "no_new_privileges=true\n",
                "pids_limit={pids}\n",
                "cpu={cpu}\n",
                "memory_mib={memory_mib}\n",
                "disk_mib={disk_mib}\n",
                "read_only=true\n",
                "tmpfs={tmpfs}\n",
                "user=65534:65534\n",
                "ephemeral=true\n",
                "workspace_mount=false\n",
                "host_environment=false\n",
                "host_volumes=false\n",
                "container_environment=false\n",
                "timeout_seconds={timeout}\n",
                "max_output_bytes={max_output}\n"
            ),
            image = image,
            pids = DEFAULT_CONTAINER_PIDS,
            cpu = cpu,
            memory_mib = memory_mib,
            disk_mib = disk_mib,
            tmpfs = CONTAINER_TMPFS,
            timeout = self.default_timeout,
            max_output = self.max_output_size,
        ))
    }

    fn docker_env_pairs(&self) -> Result<Vec<String>, AgentError> {
        self.docker_env
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let (name, _value) = entry.split_once('=').ok_or_else(|| {
                    AgentError::Config(format!(
                        "Docker environment entry must be KEY=VALUE, got '{entry}'"
                    ))
                })?;
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    || should_strip_subprocess_env_key(name, false)
                {
                    return Err(AgentError::Config(format!(
                        "Docker environment entry '{name}' is not permitted"
                    )));
                }
                Ok(entry.to_string())
            })
            .collect()
    }

    fn canonical_workspace_mount(cwd: &Path) -> Result<PathBuf, AgentError> {
        if !cwd.is_absolute() {
            return Err(AgentError::Config(
                "Manual Docker workspace mount must use an absolute host path".into(),
            ));
        }
        let canonical = cwd.canonicalize().map_err(|error| {
            AgentError::Io(format!(
                "Failed to canonicalize manual Docker workspace '{}': {error}",
                cwd.display()
            ))
        })?;
        if canonical != cwd {
            return Err(AgentError::Config(format!(
                "Manual Docker workspace '{}' traverses a symlink and is refused",
                cwd.display()
            )));
        }
        if canonical == Path::new("/") || canonical.to_string_lossy().contains(':') {
            return Err(AgentError::Config(format!(
                "Manual Docker workspace '{}' is unsafe for a volume mount",
                canonical.display()
            )));
        }

        let protected_roots = [
            std::env::var_os("HOME").map(PathBuf::from),
            std::env::var_os("HERMES_HOME").map(PathBuf::from),
            std::env::var_os("HERMES_AGENT_ULTRA_HOME").map(PathBuf::from),
        ];
        for root in protected_roots.into_iter().flatten() {
            let root = root.canonicalize().unwrap_or(root);
            if canonical == root || canonical.starts_with(&root) || root.starts_with(&canonical) {
                return Err(AgentError::Config(format!(
                    "Manual Docker workspace '{}' would mount a host home or an ancestor",
                    canonical.display()
                )));
            }
        }
        Ok(canonical)
    }

    fn build_run_args(&self, image: &str) -> Result<Vec<String>, AgentError> {
        if !self.docker_forward_env.is_empty() {
            return Err(AgentError::Config(
                "Docker host environment forwarding is disabled".into(),
            ));
        }
        if !self.docker_volumes.is_empty() {
            return Err(AgentError::Config(
                "Docker arbitrary host volumes are disabled; docker socket, root, and home mounts are not permitted".into(),
            ));
        }

        let cpu = self
            .container_cpu
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_CONTAINER_CPUS);
        let memory = self
            .container_memory
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_CONTAINER_MEMORY_MIB);
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--tty".to_string(),
            "--network".to_string(),
            "none".to_string(),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--pids-limit".to_string(),
            DEFAULT_CONTAINER_PIDS.to_string(),
            "--cpus".to_string(),
            cpu.to_string(),
            "--memory".to_string(),
            format!("{memory}m"),
            "--read-only".to_string(),
            "--tmpfs".to_string(),
            CONTAINER_TMPFS.to_string(),
        ];
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
        } else {
            args.push("--user".to_string());
            args.push("65534:65534".to_string());
        }
        if let Some(disk) = self.container_disk.filter(|value| *value > 0) {
            args.push("--storage-opt".to_string());
            args.push(format!("size={disk}m"));
        }
        for env_pair in self.docker_env_pairs()? {
            args.push("-e".to_string());
            args.push(env_pair);
        }
        if self.mount_cwd_to_workspace {
            self.manual_workspace_mount.ok_or_else(|| {
                AgentError::Config(
                    "Docker host cwd mount requires DockerManualWorkspaceMountCapability".into(),
                )
            })?;
            let cwd = std::env::current_dir().map_err(|error| {
                AgentError::Io(format!("Failed to resolve current directory for Docker: {error}"))
            })?;
            let cwd = Self::canonical_workspace_mount(&cwd)?;
            args.push("-v".to_string());
            args.push(format!("{}:/workspace:rw", cwd.display()));
            args.push("-w".to_string());
            args.push("/workspace".to_string());
        }
        args.push(image.to_string());
        args.push("tail".to_string());
        args.push("-f".to_string());
        args.push("/dev/null".to_string());
        Ok(args)
    }

    /// Ensure we have a running container. If `container_id` is None,
    /// create one from the configured image.
    async fn ensure_container(&self) -> Result<String, AgentError> {
        if let Some(id) = self
            .container_id.lock()
            .clone()
        {
            return Ok(id);
        }

        let image = self
            .image
            .as_ref()
            .ok_or_else(|| AgentError::Config("No Docker image specified".into()))?;

        tracing::info!("Creating Docker container from image: {}", image);

        let args = self.build_run_args(image)?;

        #[cfg(test)]
        if let Some(runner) = self.test_command_runner.as_ref().cloned() {
            let id = runner.create_container(&args).await?;
            *self.container_id.lock() = Some(id.clone());
            return Ok(id);
        }

        let output = TokioCommand::new("docker")
            .args(&args)
            .suppress_windows_console()
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
        *self.container_id.lock() = Some(id.clone());
        Ok(id)
    }

    fn truncate_output(&self, s: String) -> String {
        if s.len() > self.max_output_size {
            s[..self.max_output_size].to_string()
        } else {
            s
        }
    }

    fn clear_container_id(&self) {
        *self.container_id.lock() = None;
    }

    fn should_recreate_after_exec_output(&self, output: &CommandOutput) -> bool {
        self.container_persistent && output_indicates_container_gone(output)
    }

    async fn execute_in_container(
        &self,
        container_id: &str,
        command: &str,
        timeout_secs: u64,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
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

        #[cfg(test)]
        if let Some(runner) = self.test_command_runner.as_ref().cloned() {
            return tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                runner.execute_command(&args),
            )
            .await
            .map_err(|_| {
                AgentError::Timeout(format!("Docker command timed out after {timeout_secs} seconds"))
            })?;
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let output = TokioCommand::new("docker")
                .args(&args)
                .suppress_windows_console()
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
}

/// The only executable plan attached to a certified Docker context.
///
/// It uses the same hardened backend instance that produced the configuration
/// digest. A failure at either Docker launch or command execution is typed and
/// returned to the caller; this plan never delegates to a host backend.
struct DockerHardenedExecutionPlan {
    backend: Arc<DockerBackend>,
}

#[async_trait]
impl HardenedCommandPlan for DockerHardenedExecutionPlan {
    async fn execute_hardened(
        &self,
        request: ContainmentCommandRequest,
    ) -> Result<CommandOutput, HardenedCommandPlanFailure> {
        if request.stdin_data.is_some() {
            return Err(HardenedCommandPlanFailure::Execution(
                "stdin_data is not supported by the hardened Docker execution plan".to_string(),
            ));
        }
        let timeout_secs = request.timeout.unwrap_or(self.backend.default_timeout);
        let container_id = self
            .backend
            .ensure_container()
            .await
            .map_err(|error| HardenedCommandPlanFailure::Launch(error.to_string()))?;
        self.backend
            .execute_in_container(
                &container_id,
                &request.command,
                timeout_secs,
                request.workdir.as_deref(),
                request.background,
                request.pty,
            )
            .await
            .map_err(|error| HardenedCommandPlanFailure::Execution(error.to_string()))
    }
}

fn immutable_image_reference(image: &str) -> bool {
    if image.is_empty()
        || image
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return false;
    }
    let Some((reference, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    !reference.is_empty()
        && !reference.contains('@')
        && digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn container_gone_message(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("no such container")
        || lower.contains("no such object")
        || lower.contains("is not running")
}

fn output_indicates_container_gone(output: &CommandOutput) -> bool {
    output.exit_code != 0
        && (container_gone_message(&output.stderr) || container_gone_message(&output.stdout))
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
        let timeout_secs = timeout.unwrap_or(self.default_timeout);
        let mut container_id = self.ensure_container().await?;
        let mut output = self
            .execute_in_container(
                &container_id,
                command,
                timeout_secs,
                workdir,
                background,
                pty,
            )
            .await?;

        if self.should_recreate_after_exec_output(&output) {
            tracing::warn!(
                container_id = container_id,
                "Docker persistent container disappeared; recreating and retrying command once"
            );
            self.clear_container_id();
            container_id = self.ensure_container().await?;
            output = self
                .execute_in_container(
                    &container_id,
                    command,
                    timeout_secs,
                    workdir,
                    background,
                    pty,
                )
                .await?;
        }

        Ok(output)
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
            .suppress_windows_console()
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
                .suppress_windows_console()
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
            .suppress_windows_console()
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
            .suppress_windows_console()
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to check file via docker: {}", e)))?;

        // test -e returns 0 if file exists, 1 otherwise
        Ok(output.status.success())
    }
}

#[cfg(test)]
mod containment_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use hermes_core::{ExecutionMode, ExecutionModeSource};

    use super::*;
    use crate::containment::{ContainmentExecutionError, VerifiedIsolatedBackendKind};

    #[derive(Default)]
    struct FakeHardenedDockerRunner {
        launch_fails: AtomicBool,
        launch_calls: AtomicUsize,
        execute_calls: AtomicUsize,
        run_args: Mutex<Vec<Vec<String>>>,
        execute_args: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl DockerTestCommandRunner for FakeHardenedDockerRunner {
        async fn create_container(&self, args: &[String]) -> Result<String, AgentError> {
            self.launch_calls.fetch_add(1, Ordering::Relaxed);
            self.run_args.lock().push(args.to_vec());
            if self.launch_fails.load(Ordering::Relaxed) {
                return Err(AgentError::Io("fake hardened Docker launch failure".into()));
            }
            Ok("fake-contained-id".into())
        }

        async fn execute_command(&self, args: &[String]) -> Result<CommandOutput, AgentError> {
            self.execute_calls.fetch_add(1, Ordering::Relaxed);
            self.execute_args.lock().push(args.to_vec());
            Ok(CommandOutput {
                exit_code: 0,
                stdout: "contained-ok".into(),
                stderr: String::new(),
            })
        }
    }

    fn pinned_image() -> String {
        format!("registry.example/hermes@sha256:{}", "a".repeat(64))
    }

    fn reviewed_mode() -> ResolvedExecutionMode {
        ResolvedExecutionMode {
            mode: ExecutionMode::Reviewed,
            source: ExecutionModeSource::Cli,
        }
    }

    fn fresh_hardened_backend(runner: Arc<dyn DockerTestCommandRunner>) -> Arc<DockerBackend> {
        Arc::new(
            DockerBackend::new(
                None,
                Some(pinned_image()),
                false,
                false,
                None,
                None,
                None,
                false,
                None,
                Vec::new(),
                Vec::new(),
                30,
                1024,
            )
            .with_test_command_runner(runner),
        )
    }

    fn request() -> ContainmentCommandRequest {
        ContainmentCommandRequest {
            command: "printf contained".into(),
            timeout: Some(5),
            workdir: None,
            background: false,
            pty: false,
            stdin_data: None,
        }
    }

    #[tokio::test]
    async fn exact_fresh_hardened_docker_context_reaches_fake_runner_and_records_receipt() {
        let runner = Arc::new(FakeHardenedDockerRunner::default());
        let backend = fresh_hardened_backend(runner.clone());
        let context = backend
            .closed_execution_context(reviewed_mode())
            .expect("pinned fresh hardened Docker backend is certifiable");
        let certificate = context.certificate().expect("closed certificate");
        assert_eq!(certificate.backend_kind(), VerifiedIsolatedBackendKind::Docker);
        assert_eq!(context.sandbox_digest(), Some(certificate.configuration_digest()));

        let result = context
            .execute_host_sensitive(
                context
                    .prepare_host_sensitive_invocation("terminal")
                    .expect("opaque invocation"),
                request(),
            )
            .await
            .expect("fake hardened execution");

        assert_eq!(result.output.stdout, "contained-ok");
        assert_eq!(runner.launch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runner.execute_calls.load(Ordering::Relaxed), 1);
        let run_args = runner.run_args.lock();
        assert!(run_args[0].windows(2).any(|pair| pair == ["--network", "none"]));
        assert!(run_args[0].windows(2).any(|pair| pair == ["--cap-drop", "ALL"]));
        assert!(run_args[0].iter().any(|argument| argument == "--read-only"));
        assert!(!run_args[0].iter().any(|argument| argument == "-v"));
        drop(run_args);
        assert_eq!(context.completed_execution_receipts().len(), 1);
        assert_eq!(
            result.receipt.launch.configuration_digest,
            certificate.configuration_digest()
        );
    }

    #[tokio::test]
    async fn hardened_docker_launch_failure_has_no_execute_or_host_fallback() {
        let runner = Arc::new(FakeHardenedDockerRunner::default());
        runner.launch_fails.store(true, Ordering::Relaxed);
        let context = fresh_hardened_backend(runner.clone())
            .closed_execution_context(reviewed_mode())
            .expect("certified context");
        let error = context
            .execute_host_sensitive(
                context
                    .prepare_host_sensitive_invocation("terminal")
                    .expect("opaque invocation"),
                request(),
            )
            .await
            .expect_err("launch failure must not execute elsewhere");

        assert!(matches!(error, ContainmentExecutionError::LaunchFailed { .. }));
        assert_eq!(runner.launch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runner.execute_calls.load(Ordering::Relaxed), 0);
        assert!(context.completed_execution_receipts().is_empty());
    }

    #[test]
    fn certificate_rejects_changed_configuration_and_untrusted_backends() {
        let runner = Arc::new(FakeHardenedDockerRunner::default());
        let original = fresh_hardened_backend(runner.clone());
        let certificate = original
            .verified_isolated_backend_capability()
            .expect("original certificate");
        let changed = Arc::new(
            DockerBackend::new(
                None,
                Some(pinned_image()),
                false,
                false,
                Some(2),
                None,
                None,
                false,
                None,
                Vec::new(),
                Vec::new(),
                30,
                1024,
            )
            .with_test_command_runner(runner),
        );
        assert_eq!(
            changed
                .closed_execution_context_with_capability(reviewed_mode(), certificate)
                .expect_err("changed configuration must not consume a certificate"),
            DockerContainmentCertificationError::CertificateConfigurationMismatch
        );

        let existing = Arc::new(DockerBackend::new(
            Some("external-container".into()),
            Some(pinned_image()),
            false,
            false,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
            Vec::new(),
            30,
            1024,
        ));
        assert_eq!(
            existing.verified_isolated_backend_capability().unwrap_err(),
            DockerContainmentCertificationError::ExistingContainer
        );
        let host_mount = DockerBackend::new(
            None,
            Some(pinned_image()),
            true,
            false,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
            Vec::new(),
            30,
            1024,
        );
        assert_eq!(
            host_mount.verified_isolated_backend_capability().unwrap_err(),
            DockerContainmentCertificationError::HostWorkspaceMount
        );
    }
}

#[cfg(test)]
mod quote_tests {
    use super::*;

    #[test]
    fn build_run_args_use_least_privilege_defaults() {
        let backend = DockerBackend::new(
            None,
            Some("rust:1.90".to_string()),
            false,
            false,
            None,
            None,
            None,
            false,
            Some("FOO=bar".to_string()),
            Vec::new(),
            Vec::new(),
            120,
            1_048_576,
        );

        let args = backend
            .build_run_args("rust:1.90")
            .expect("least-privilege args");
        assert!(args.windows(2).any(|window| window == ["--network", "none"]));
        assert!(args.windows(2).any(|window| window == ["--cap-drop", "ALL"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--security-opt", "no-new-privileges"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--pids-limit", "256"]));
        assert!(args.windows(2).any(|window| window == ["--cpus", "1"]));
        assert!(args.windows(2).any(|window| window == ["--memory", "512m"]));
        assert!(args.iter().any(|arg| arg == "--read-only"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--tmpfs", CONTAINER_TMPFS]));
        assert!(args.windows(2).any(|window| window == ["-e", "FOO=bar"]));
        assert!(args.windows(2).any(|window| window == ["--user", "65534:65534"]));
        assert!(args.iter().any(|arg| arg == "--rm"));
        assert!(!args.iter().any(|arg| arg == "-v"));
        assert_eq!(
            backend.containment_proof(),
            DockerContainmentProof::LeastPrivilegeContainer
        );
    }

    #[test]
    fn build_run_args_refuse_credential_forwarding_and_host_volumes() {
        let forward = DockerBackend::new(
            None,
            None,
            false,
            false,
            None,
            None,
            None,
            false,
            None,
            vec!["OPENAI_API_KEY".to_string()],
            Vec::new(),
            30,
            1024,
        );
        assert!(forward.build_run_args("ubuntu:22.04").is_err());

        let socket_mount = DockerBackend::new(
            None,
            None,
            false,
            false,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
            vec!["/var/run/docker.sock:/var/run/docker.sock".to_string()],
            30,
            1024,
        );
        assert!(socket_mount.build_run_args("ubuntu:22.04").is_err());

        let credential_env = DockerBackend::new(
            None,
            None,
            false,
            false,
            None,
            None,
            None,
            false,
            Some("OPENAI_API_KEY=secret".to_string()),
            Vec::new(),
            Vec::new(),
            30,
            1024,
        );
        assert!(credential_env.build_run_args("ubuntu:22.04").is_err());
    }

    #[test]
    fn workspace_mount_requires_typed_manual_capability() {
        let backend = DockerBackend::new(
            None,
            None,
            true,
            false,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
            Vec::new(),
            30,
            1024,
        );
        assert!(backend.build_run_args("ubuntu:22.04").is_err());
        assert!(DockerBackend::canonical_workspace_mount(Path::new("/")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_mount_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let link = tmp.path().join("workspace-link");
        symlink(&workspace, &link).expect("workspace symlink");
        assert!(DockerBackend::canonical_workspace_mount(&link).is_err());
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

    #[test]
    fn detects_docker_container_removed_or_stopped_messages() {
        let missing = CommandOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "Error response from daemon: No such container: abc".to_string(),
        };
        assert!(output_indicates_container_gone(&missing));

        let stopped = CommandOutput {
            exit_code: 1,
            stdout: "Container abc is not running".to_string(),
            stderr: String::new(),
        };
        assert!(output_indicates_container_gone(&stopped));

        let normal_failure = CommandOutput {
            exit_code: 2,
            stdout: String::new(),
            stderr: "command not found".to_string(),
        };
        assert!(!output_indicates_container_gone(&normal_failure));

        let successful_text_match = CommandOutput {
            exit_code: 0,
            stdout: "No such container appeared in command output".to_string(),
            stderr: String::new(),
        };
        assert!(!output_indicates_container_gone(&successful_text_match));
    }

    #[test]
    fn retry_after_missing_container_is_persistent_only() {
        let gone = CommandOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "No such object: abc".to_string(),
        };
        let persistent = DockerBackend::new(
            None,
            Some("ubuntu:22.04".to_string()),
            false,
            false,
            None,
            None,
            None,
            true,
            None,
            Vec::new(),
            Vec::new(),
            30,
            1024,
        );
        assert!(persistent.should_recreate_after_exec_output(&gone));

        let ephemeral = DockerBackend::new(
            None,
            Some("ubuntu:22.04".to_string()),
            false,
            false,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
            Vec::new(),
            30,
            1024,
        );
        assert!(!ephemeral.should_recreate_after_exec_output(&gone));
    }
}
