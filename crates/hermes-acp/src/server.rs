//! ACP server (JSON-RPC over stdin/stdout).

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::handler::AcpHandler;
use crate::protocol::AcpRequest;

/// ACP JSON-RPC server.
pub struct AcpServer {
    handler: Arc<dyn AcpHandler>,
}

impl AcpServer {
    pub fn new(handler: Arc<dyn AcpHandler>) -> Self {
        Self { handler }
    }

    /// Run the server, reading JSON-RPC requests from stdin and writing responses to stdout.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        tracing::info!("ACP server started, listening on stdin");

        while let Some(line) = lines.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let request: AcpRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let error_resp = crate::protocol::AcpResponse::error(
                        None,
                        -32700,
                        format!("Parse error: {}", e),
                    );
                    let json = serde_json::to_string(&error_resp)?;
                    stdout.write_all(json.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                    continue;
                }
            };

            let response = self.handler.handle_request(request).await;
            let json = serde_json::to_string(&response)?;
            stdout.write_all(json.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }

        Ok(())
    }
}
