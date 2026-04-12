//! Daytona terminal backend – executes commands via the Daytona API.

use async_trait::async_trait;
use reqwest::Client;

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

/// A [`TerminalBackend`] that delegates to a Daytona workspace via its REST API.
pub struct DaytonaBackend {
    /// Base URL for the Daytona API (e.g. "https://api.daytona.io").
    api_url: String,
    /// API key for authentication.
    api_key: Option<String>,
    /// Workspace ID to target.
    workspace_id: Option<String>,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes.
    max_output_size: usize,
    /// HTTP client for making API requests.
    client: Client,
}

impl DaytonaBackend {
    /// Create a new Daytona backend.
    ///
    /// - `api_url`: If None, falls back to the `DAYTONA_API_URL` env var or
    ///   `https://api.daytona.io`.
    /// - `api_key`: If None, falls back to the `DAYTONA_API_KEY` env var.
    /// - `workspace_id`: If None, falls back to the `DAYTONA_WORKSPACE_ID` env var.
    pub fn new(
        api_url: Option<String>,
        api_key: Option<String>,
        workspace_id: Option<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        let api_url = api_url
            .or_else(|| std::env::var("DAYTONA_API_URL").ok())
            .unwrap_or_else(|| "https://api.daytona.io".to_string());

        let api_key = api_key.or_else(|| std::env::var("DAYTONA_API_KEY").ok());
        let workspace_id = workspace_id.or_else(|| std::env::var("DAYTONA_WORKSPACE_ID").ok());

        Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key,
            workspace_id,
            default_timeout,
            max_output_size,
            client: Client::new(),
        }
    }

    /// Get the workspace ID, returning an error if not configured.
    fn workspace_id(&self) -> Result<&str, AgentError> {
        self.workspace_id
            .as_deref()
            .ok_or_else(|| AgentError::Config("Daytona workspace ID not configured".into()))
    }

    /// Build an authenticated request builder for the given method and path.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.api_url, path);
        let mut req = self.client.request(method, &url);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
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

/// Request body for executing a command.
#[derive(serde::Serialize)]
struct ExecuteRequest {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
}

/// Response body from executing a command.
#[derive(serde::Deserialize)]
struct ExecuteResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Response body for file existence check.
#[derive(serde::Deserialize)]
struct FileExistsResponse {
    exists: bool,
}

#[async_trait]
impl TerminalBackend for DaytonaBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let workspace_id = self.workspace_id()?;
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        let path = format!("/workspace/{}/execute", workspace_id);
        let body = ExecuteRequest {
            command: command.to_string(),
            workdir: workdir.map(|s| s.to_string()),
            timeout: Some(timeout_secs),
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs + 10), // Add buffer for network
            async {
                let resp = self
                    .request(reqwest::Method::POST, &path)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| AgentError::Io(format!("Daytona API request failed: {}", e)))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return Err(AgentError::Io(format!(
                        "Daytona API returned status {}: {}",
                        status, text
                    )));
                }

                let data: ExecuteResponse = resp
                    .json()
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to parse Daytona response: {}", e)))?;

                Ok(CommandOutput {
                    exit_code: data.exit_code,
                    stdout: self.truncate_output(data.stdout),
                    stderr: self.truncate_output(data.stderr),
                })
            },
        )
        .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "Daytona command timed out after {} seconds",
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
        let workspace_id = self.workspace_id()?;
        let api_path = format!(
            "/workspace/{}/file?path={}&offset={}&limit={}",
            workspace_id,
            urlencoding(path),
            offset.unwrap_or(0),
            limit.unwrap_or(0),
        );

        let resp = self
            .request(reqwest::Method::GET, &api_path)
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Daytona read_file request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Daytona read_file returned status {}: {}",
                status, text
            )));
        }

        let content = resp
            .text()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to read Daytona response: {}", e)))?;

        Ok(content)
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        let workspace_id = self.workspace_id()?;
        let api_path = format!(
            "/workspace/{}/file?path={}",
            workspace_id,
            urlencoding(path),
        );

        let resp = self
            .request(reqwest::Method::PUT, &api_path)
            .header("Content-Type", "application/octet-stream")
            .body(content.to_string())
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Daytona write_file request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Daytona write_file returned status {}: {}",
                status, text
            )));
        }

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let workspace_id = self.workspace_id()?;
        let api_path = format!(
            "/workspace/{}/file/exists?path={}",
            workspace_id,
            urlencoding(path),
        );

        let resp = self
            .request(reqwest::Method::GET, &api_path)
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Daytona file_exists request failed: {}", e)))?;

        if !resp.status().is_success() {
            // If we get a 404, the file doesn't exist
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(false);
            }
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Io(format!(
                "Daytona file_exists returned status {}: {}",
                status, text
            )));
        }

        let data: FileExistsResponse = resp
            .json()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to parse Daytona response: {}", e)))?;

        Ok(data.exists)
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