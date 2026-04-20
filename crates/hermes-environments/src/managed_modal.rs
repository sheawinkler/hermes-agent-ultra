//! Managed Modal environment that handles workspace lifecycle.
//!
//! Two transports are supported:
//!
//! 1. [`ModalTransport::DirectApi`] — talks to `https://api.modal.com/v1/...`
//!    with the user's `MODAL_API_TOKEN` (or token passed at construction).
//!    This is the "I bring my own Modal credentials" path.
//!
//! 2. [`ModalTransport::Managed`] — talks to a Nous-hosted gateway resolved
//!    via [`hermes_config::managed_gateway::resolve_managed_tool_gateway`]
//!    for the `"modal"` vendor, using a Nous OAuth bearer token. This is
//!    the path used by Hermes managed-mode users who don't have their own
//!    Modal account but do have a Nous subscription.
//!
//! The active transport is reflected in [`ManagedModalBackend::transport_label`]
//! for observability and tests.

use async_trait::async_trait;
use reqwest::Client;

use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use hermes_core::{AgentError, CommandOutput, TerminalBackend};

const MODAL_API_ROOT: &str = "https://api.modal.com";

/// Identifies how a Modal workspace request reaches the Modal control
/// plane. Returned via [`ManagedModalBackend::transport_label`] for logging
/// and integration tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalTransport {
    /// Direct call to `https://api.modal.com/v1/...` with the user's Modal
    /// API token (`MODAL_API_TOKEN` or constructor-supplied).
    DirectApi { api_token: String },
    /// Routed through a Nous-managed gateway (`vendor = "modal"`) with a
    /// Nous OAuth bearer.
    Managed {
        gateway_origin: String,
        nous_token: String,
    },
}

impl ModalTransport {
    pub fn label(&self) -> &'static str {
        match self {
            Self::DirectApi { .. } => "direct",
            Self::Managed { .. } => "managed",
        }
    }

    fn root(&self) -> &str {
        match self {
            Self::DirectApi { .. } => MODAL_API_ROOT,
            Self::Managed { gateway_origin, .. } => gateway_origin.as_str(),
        }
    }

    fn bearer(&self) -> &str {
        match self {
            Self::DirectApi { api_token } => api_token,
            Self::Managed { nous_token, .. } => nous_token,
        }
    }

    /// Endpoint URL for a given (versioned) Modal API path, e.g. `/v1/workspaces`.
    /// Strips a single trailing slash from the root and joins on `/`.
    pub fn endpoint(&self, path: &str) -> String {
        let root = self.root().trim_end_matches('/');
        let suffix = path.trim_start_matches('/');
        format!("{root}/{suffix}")
    }
}

/// A managed Modal backend that creates and destroys workspaces on demand.
#[derive(Debug)]
pub struct ManagedModalBackend {
    transport: ModalTransport,
    workspace_id: Option<String>,
    gpu_type: Option<String>,
    default_timeout: u64,
    max_output_size: usize,
    client: Client,
}

impl ManagedModalBackend {
    /// Construct a direct-API backend with the user's own Modal token.
    pub fn new(api_key: &str) -> Self {
        Self {
            transport: ModalTransport::DirectApi {
                api_token: api_key.to_string(),
            },
            workspace_id: None,
            gpu_type: None,
            default_timeout: 300,
            max_output_size: 1_048_576,
            client: Client::new(),
        }
    }

    /// Construct a managed-mode backend from an already-resolved gateway
    /// config (typically returned by `resolve_managed_tool_gateway("modal")`).
    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            transport: ModalTransport::Managed {
                gateway_origin: cfg.gateway_origin.trim_end_matches('/').to_string(),
                nous_token: cfg.nous_user_token.clone(),
            },
            workspace_id: None,
            gpu_type: None,
            default_timeout: 300,
            max_output_size: 1_048_576,
            client: Client::new(),
        }
    }

    /// Resolve the best-available transport from the environment.
    ///
    /// Priority:
    /// 1. `MODAL_API_TOKEN` env var → `DirectApi`
    /// 2. Nous-managed `modal` vendor (requires
    ///    `HERMES_ENABLE_NOUS_MANAGED_TOOLS` and a Nous OAuth token) →
    ///    `Managed`
    /// 3. `Err(AgentError::Io)` with a message covering both paths.
    pub fn from_env_or_managed() -> Result<Self, AgentError> {
        if let Ok(token) = std::env::var("MODAL_API_TOKEN") {
            let trimmed = token.trim();
            if !trimmed.is_empty() {
                return Ok(Self::new(trimmed));
            }
        }
        if let Some(cfg) = resolve_managed_tool_gateway("modal", ResolveOptions::default()) {
            return Ok(Self::from_managed(&cfg));
        }
        Err(AgentError::Io(
            "MODAL_API_TOKEN not set and Nous-managed `modal` gateway is not configured. \
             Set MODAL_API_TOKEN, or enable HERMES_ENABLE_NOUS_MANAGED_TOOLS with a Nous \
             OAuth token (TOOL_GATEWAY_USER_TOKEN or auth.json)."
                .into(),
        ))
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

    /// Reports the active transport (`"direct"` or `"managed"`). Useful for
    /// tests and structured logs.
    pub fn transport_label(&self) -> &'static str {
        self.transport.label()
    }

    /// Reports the active transport enum (mostly for tests).
    pub fn transport(&self) -> &ModalTransport {
        &self.transport
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
            .post(self.transport.endpoint("/v1/workspaces"))
            .bearer_auth(self.transport.bearer())
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

        let id = data["id"].as_str().unwrap_or("unknown").to_string();

        self.workspace_id = Some(id.clone());
        tracing::info!(
            transport = self.transport.label(),
            workspace_id = %id,
            "Created managed Modal workspace"
        );
        Ok(id)
    }

    /// Destroy the current workspace and release resources.
    pub async fn destroy_workspace(&mut self) -> Result<(), AgentError> {
        let id = match self.workspace_id.take() {
            Some(id) => id,
            None => return Ok(()),
        };

        let url = self.transport.endpoint(&format!("/v1/workspaces/{}", id));
        let resp = self
            .client
            .delete(&url)
            .bearer_auth(self.transport.bearer())
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to destroy workspace: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!(
                transport = self.transport.label(),
                status = %status,
                body = %text,
                "Modal workspace destruction returned non-2xx"
            );
        } else {
            tracing::info!(
                transport = self.transport.label(),
                workspace_id = %id,
                "Destroyed managed Modal workspace"
            );
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
                .post(self.transport.endpoint("/v1/execute"))
                .bearer_auth(self.transport.bearer())
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
            stdout: self.truncate_output(data["stdout"].as_str().unwrap_or("").to_string()),
            stderr: self.truncate_output(data["stderr"].as_str().unwrap_or("").to_string()),
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
            .execute_command(
                &format!("cat {}", shell_escape(path)),
                None,
                None,
                false,
                false,
            )
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
        let output = self.execute_command(&cmd, None, None, false, false).await?;
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

#[cfg(test)]
mod managed_modal_tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    /// Hermetic env scope: temp `HERMES_HOME` + cleared modal/managed env vars.
    /// Holds the global env-var lock for serialised concurrent tests.
    struct EnvScope {
        _tmp: tempfile::TempDir,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMES_HOME",
                "MODAL_API_TOKEN",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                std::env::remove_var(k);
            }
            std::env::set_var("HERMES_HOME", tmp.path());
            Self {
                _tmp: tmp,
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn direct_transport_uses_modal_api_root() {
        let b = ManagedModalBackend::new("tok-direct");
        assert_eq!(b.transport_label(), "direct");
        assert_eq!(
            b.transport.endpoint("/v1/workspaces"),
            "https://api.modal.com/v1/workspaces"
        );
        assert_eq!(b.transport.bearer(), "tok-direct");
    }

    #[test]
    fn managed_transport_uses_resolved_origin_and_token() {
        let cfg = ManagedToolGatewayConfig {
            vendor: "modal".into(),
            gateway_origin: "https://modal.gw.example.com/".into(),
            nous_user_token: "nous-tok".into(),
            managed_mode: true,
        };
        let b = ManagedModalBackend::from_managed(&cfg);
        assert_eq!(b.transport_label(), "managed");
        assert_eq!(
            b.transport.endpoint("/v1/workspaces"),
            "https://modal.gw.example.com/v1/workspaces"
        );
        assert_eq!(
            b.transport.endpoint("/v1/workspaces/abc-123"),
            "https://modal.gw.example.com/v1/workspaces/abc-123"
        );
        assert_eq!(b.transport.bearer(), "nous-tok");
    }

    #[test]
    fn endpoint_handles_leading_and_trailing_slashes() {
        let b = ManagedModalBackend::new("k");
        assert_eq!(
            b.transport.endpoint("v1/execute"),
            "https://api.modal.com/v1/execute"
        );
        assert_eq!(
            b.transport.endpoint("/v1/execute"),
            "https://api.modal.com/v1/execute"
        );
    }

    #[test]
    fn from_env_or_managed_prefers_direct_token() {
        let _g = EnvScope::new();
        std::env::set_var("MODAL_API_TOKEN", "direct-tok");
        let b = ManagedModalBackend::from_env_or_managed().unwrap();
        assert_eq!(b.transport_label(), "direct");
        assert_eq!(b.transport.bearer(), "direct-tok");
    }

    #[test]
    fn from_env_or_managed_falls_back_to_managed_gateway() {
        let _g = EnvScope::new();
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-fallback");
        let b = ManagedModalBackend::from_env_or_managed().unwrap();
        assert_eq!(b.transport_label(), "managed");
        assert_eq!(b.transport.bearer(), "nous-fallback");
        assert!(b
            .transport
            .endpoint("/v1/workspaces")
            .ends_with("/v1/workspaces"));
    }

    #[test]
    fn from_env_or_managed_errors_when_neither_configured() {
        let _g = EnvScope::new();
        let err = ManagedModalBackend::from_env_or_managed().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("MODAL_API_TOKEN"), "unexpected: {msg}");
        assert!(msg.contains("modal"), "unexpected: {msg}");
    }

    #[test]
    fn empty_direct_token_falls_through_to_error_when_no_managed() {
        let _g = EnvScope::new();
        std::env::set_var("MODAL_API_TOKEN", "   ");
        let err = ManagedModalBackend::from_env_or_managed().unwrap_err();
        assert!(err.to_string().contains("MODAL_API_TOKEN"));
    }

    #[test]
    fn with_gpu_and_timeout_chain_through_construction() {
        let b = ManagedModalBackend::new("k")
            .with_gpu("A100")
            .with_timeout(123);
        assert_eq!(b.gpu_type.as_deref(), Some("A100"));
        assert_eq!(b.default_timeout, 123);
    }
}
