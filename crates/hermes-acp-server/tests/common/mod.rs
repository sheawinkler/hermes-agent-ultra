//! Shared test helpers for ACP server integration tests.

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Trait alias for a bidirectional async stream used in tests.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// Platform-specific IPC endpoint path for tests.
pub fn test_pipe(name: &str) -> String {
    if cfg!(windows) {
        format!(r"\\.\pipe\test-acp-{}", name)
    } else {
        format!("/tmp/test-acp-{}.sock", name)
    }
}

/// Connect to a test server with retry, removing the need for fixed sleeps.
pub async fn connect_client(pipe: &str) -> Box<dyn AsyncReadWrite> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);

    loop {
        if std::time::Instant::now() > deadline {
            panic!("failed to connect to {} within 5s", pipe);
        }

        #[cfg(windows)]
        {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe) {
                Ok(client) => return Box::new(client),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }

        #[cfg(unix)]
        {
            match tokio::net::UnixStream::connect(pipe).await {
                Ok(client) => return Box::new(client),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    }
}

/// Send a JSON-RPC request and read the first NDJSON response line.
pub async fn roundtrip(client: &mut dyn AsyncReadWrite, req: Value) -> Value {
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    client.write_all(line.as_bytes()).await.unwrap();
    client.flush().await.unwrap();

    let mut buf = vec![0u8; 4096];
    let mut total = String::new();
    loop {
        let n = client.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        total.push_str(&String::from_utf8_lossy(&buf[..n]));
        if total.contains('\n') {
            break;
        }
    }
    let line = total.split('\n').next().unwrap_or("");
    serde_json::from_str::<Value>(line).unwrap()
}

/// Send a JSON-RPC request without waiting for a response.
pub async fn send_request(client: &mut dyn AsyncReadWrite, req: Value) {
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    client.write_all(line.as_bytes()).await.unwrap();
    client.flush().await.unwrap();
}

/// Read all NDJSON lines from the client until a read timeout or EOF.
pub async fn read_all_ndjson(
    client: &mut dyn AsyncReadWrite,
    timeout: std::time::Duration,
) -> Vec<Value> {
    let start = std::time::Instant::now();
    let mut messages = Vec::new();
    let mut buffer = vec![0u8; 16384];
    let mut leftover = String::new();

    loop {
        if start.elapsed() > timeout {
            break;
        }

        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            client.read(&mut buffer),
        )
        .await
        {
            Ok(Ok(n)) if n > 0 => {
                leftover.push_str(&String::from_utf8_lossy(&buffer[..n]));
                while let Some(pos) = leftover.find('\n') {
                    let line = leftover[..pos].trim().to_string();
                    leftover = leftover[pos + 1..].to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(msg) = serde_json::from_str::<Value>(&line) {
                        messages.push(msg);
                    }
                }
            }
            Ok(Ok(_)) => break,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    messages
}
