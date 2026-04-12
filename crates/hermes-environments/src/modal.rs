//! Modal terminal backend – executes commands via the Modal API.

use async_trait::async_trait;
use reqwest::Client;

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

/// GPU environment selection for Modal backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuType {
    /// No GPU – CPU only.
    None,
    /// NVIDIA T4 GPU.
    T4,
    /// NVIDIA A10G GPU.
    A10G,
    /// NVIDIA A100 GPU.
    A100,
    /// NVIDIA H100 GPU.
    H100,
}

impl std::fmt::Display for GpuType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuType::None => write!(f, "cpu"),
            GpuType::T4 => write!(f, "T4"),
            GpuType::A10G => write!(f, "A10G"),
            GpuType::A100 => write!(f, "A100"),
            GpuType::H100 => write!(f, "H100"),
        }
    }
}

/// A [`TerminalBackend`] that runs commands on Modal via its REST API.
pub struct ModalBackend {
    /// Modal app name.
    app_name: Option<String>,
    /// API token for Modal authentication.
    api_token: Option<String>,
    /// GPU type for the environment.
    gpu: GpuType,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes.
    max_output_size: usize,
    /// HTTP client.
    client: Client,
}

impl ModalBackend {
    /// Create a new Modal backend.
    ///
    /// - `app_name`: If None, falls back to `MODAL_APP_NAME` env var or "hermes-agent".
    /// - `api_token`: If None, falls back to `MODAL_API_TOKEN` env var.
    pub fn new(
        app_name: Option<String>,
        api_token: Option<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        let api_token = api_token.or_else(|| std::env::var("MODAL_API_TOKEN").ok());

        Self {
            app_name,
            api_token,
            gpu: GpuType::None,
            default_timeout,
            max_output_size,
            client: Client::new(),
        }
    }

    /// Set the GPU type for this backend.
    pub fn with_gpu(mut self, gpu: GpuType) -> Self {
        self.gpu = gpu;
        self
    }

    /// Get the app name, falling back to a default.
    fn app_name(&self) -> String {
        self.app_name
            .clone()
            .or_else(|| std::env::var("MODAL_APP_NAME").ok())
            .unwrap_or_else(|| "hermes-agent".to_string())
    }

    /// Build an authenticated request builder.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.client.request(method, url);
        if let Some(ref token) = self.api_token {
            req = req.bearer_auth(token);
        }
        req
    }

    fn truncate_output(&self, s: String) -> String {
        if s.len() > self.max_output_size {
            s[..self.max_output_size].to_string()
        } else {
            s
        }
    }
}

/// Request body for executing a command via Modal.
#[derive(serde::Serialize)]
struct ModalExecuteRequest {
    command: String,
    app_name: String,
    gpu: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
}

/// Response body from Modal command execution.
#[derive(serde::Deserialize)]
struct ModalExecuteResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Response body for file operations.
#[derive(serde::Deserialize)]
struct ModalFileResponse {
    content: Option<String>,
    exists: Option<bool>,
}

#[async_trait]
impl TerminalBackend for ModalBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        let url = "https://api.modal.com/v1/execute".to_string();
        let body = ModalExecuteRequest {
            command: command.to_string(),
            app_name: self.app_name(),
            gpu: self.gpu.to_string(),
            workdir: workdir.map(|s| s.to_string()),
            timeout: Some(timeout_secs),
        };

        let result =
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs + 10), async {
                let resp = self
                    .request(reqwest::Method::POST, &url)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| AgentError::Io(format!("Modal API request failed: {}", e)))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return Err(AgentError::Io(format!(
                        "Modal API returned status {}: {}",
                        status, text
                    )));
                }

                let data: ModalExecuteResponse = resp.json().await.map_err(|e| {
                    AgentError::Io(format!("Failed to parse Modal response: {}", e))
                })?;

                Ok(CommandOutput {
                    exit_code: data.exit_code,
                    stdout: self.truncate_output(data.stdout),
                    stderr: self.truncate_output(data.stderr),
                })
            })
            .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "Modal command timed out after {} seconds",
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
        let url = format!(
            "https://api.modal.com/v1/file?app_name={}&path={}&offset={}&limit={}",
            urlencoding(&self.app_name()),
            urlencoding(path),
            offset.unwrap_or(0),
            limit.unwrap_or(0),
        );

        let timeout_secs = self.default_timeout;
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.request(reqwest::Method::GET, &url).send(),
        )
        .await
        .map_err(|_| AgentError::Timeout("Modal read_file request timed out".into()))?
        .map_err(|e| AgentError::Io(format!("Modal read_file request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Modal read_file returned status {}: {}",
                status, text
            )));
        }

        let data: ModalFileResponse = resp
            .json()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to parse Modal response: {}", e)))?;

        Ok(data.content.unwrap_or_default())
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        let url = format!(
            "https://api.modal.com/v1/file?app_name={}&path={}",
            urlencoding(&self.app_name()),
            urlencoding(path),
        );

        let timeout_secs = self.default_timeout;
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.request(reqwest::Method::PUT, &url)
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({ "content": content }))
                .send(),
        )
        .await
        .map_err(|_| AgentError::Timeout("Modal write_file request timed out".into()))?
        .map_err(|e| AgentError::Io(format!("Modal write_file request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Modal write_file returned status {}: {}",
                status, text
            )));
        }

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let url = format!(
            "https://api.modal.com/v1/file/exists?app_name={}&path={}",
            urlencoding(&self.app_name()),
            urlencoding(path),
        );

        let timeout_secs = self.default_timeout;
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.request(reqwest::Method::GET, &url).send(),
        )
        .await
        .map_err(|_| AgentError::Timeout("Modal file_exists request timed out".into()))?
        .map_err(|e| AgentError::Io(format!("Modal file_exists request failed: {}", e)))?;

        if !resp.status().is_success() {
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(false);
            }
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Modal file_exists returned status {}: {}",
                status, text
            )));
        }

        let data: ModalFileResponse = resp
            .json()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to parse Modal response: {}", e)))?;

        Ok(data.exists.unwrap_or(false))
    }
}

/// Minimal percent-encoding for URL paths.
fn urlencoding(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('/', "%2F")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace('&', "%26")
        .replace('=', "%3D")
}
