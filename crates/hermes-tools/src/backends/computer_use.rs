//! `cua-driver` MCP backend for the Rust `computer_use` tool.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use hermes_core::{subprocess::CommandNoWindowExt, ToolError};

use crate::tools::computer_use::ComputerUseBackend;

const DEFAULT_CUA_DRIVER_CMD: &str = "cua-driver";
const DEFAULT_MCP_ARG: &str = "mcp";
const DEFAULT_MCP_TIMEOUT_SECS: u64 = 20;
const MANIFEST_TIMEOUT_SECS: u64 = 6;

#[derive(Debug, Clone)]
pub struct CuaDriverBackend {
    command: String,
    args: Vec<String>,
    timeout: Duration,
    resolve_manifest: bool,
}

impl CuaDriverBackend {
    pub fn from_env() -> Self {
        let driver = cua_driver_command_from_env();
        Self {
            command: driver,
            args: vec![DEFAULT_MCP_ARG.to_string()],
            timeout: Duration::from_secs(DEFAULT_MCP_TIMEOUT_SECS),
            resolve_manifest: true,
        }
    }

    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            timeout: Duration::from_secs(DEFAULT_MCP_TIMEOUT_SECS),
            resolve_manifest: false,
        }
    }

    pub fn command_available_from_env() -> bool {
        command_available(&cua_driver_command_from_env())
    }

    async fn call_tool_inner(&self, tool: &str, arguments: Value) -> Result<Value, ToolError> {
        let (command, args) = if self.resolve_manifest {
            resolve_mcp_invocation(&self.command).await
        } else {
            (self.command.clone(), self.args.clone())
        };
        let mut child = Command::new(&command)
            .args(&args)
            .env("CUA_DRIVER_TELEMETRY", "off")
            .env("CUA_DRIVER_DISABLE_TELEMETRY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .suppress_windows_console()
            .spawn()
            .map_err(|err| {
                ToolError::ExecutionFailed(format!(
                    "Failed to start cua-driver MCP backend '{}': {err}",
                    command
                ))
            })?;

        let mut stdin = child.stdin.take().ok_or_else(|| {
            ToolError::ExecutionFailed("cua-driver stdin was not available".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ToolError::ExecutionFailed("cua-driver stdout was not available".into())
        })?;
        let mut stdout = BufReader::new(stdout).lines();
        let stderr_task = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut stderr_text = String::new();
                let _ =
                    tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut stderr_text).await;
                stderr_text
            })
        });

        let run = async {
            write_json_line(
                &mut stdin,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {}
                }),
            )
            .await?;
            let _ = read_json_response(&mut stdout, 1).await?;

            write_json_line(
                &mut stdin,
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": tool,
                        "arguments": arguments,
                    }
                }),
            )
            .await?;
            read_json_response(&mut stdout, 2).await
        };

        let response = match tokio::time::timeout(self.timeout, run).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                if let Some(task) = stderr_task {
                    let _ = task.await;
                }
                return Err(ToolError::Timeout(format!(
                    "cua-driver MCP call '{tool}' timed out after {}s",
                    self.timeout.as_secs()
                )));
            }
        };
        let _ = stdin.shutdown().await;

        let status = child.wait().await.ok();
        let stderr = if let Some(task) = stderr_task {
            task.await.unwrap_or_default()
        } else {
            String::new()
        };

        let response = response?;
        if let Some(error) = response.get("error") {
            return Err(ToolError::ExecutionFailed(format!(
                "cua-driver MCP error for '{tool}': {error}"
            )));
        }
        if status.is_some_and(|status| !status.success()) && response.get("result").is_none() {
            return Err(ToolError::ExecutionFailed(format!(
                "cua-driver exited unsuccessfully for '{tool}': {}",
                stderr_tail(&stderr)
            )));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[async_trait]
impl ComputerUseBackend for CuaDriverBackend {
    async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, ToolError> {
        self.call_tool_inner(tool, arguments).await
    }
}

pub fn cua_driver_command_from_env() -> String {
    std::env::var("HERMES_CUA_DRIVER_CMD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_CUA_DRIVER_CMD.to_string())
}

pub fn command_available(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    let path = Path::new(command);
    if path.components().count() > 1 || path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(command))
                .any(|candidate| executable_candidate_exists(&candidate))
        })
        .unwrap_or(false)
}

fn executable_candidate_exists(path: &Path) -> bool {
    if path.is_file() {
        return true;
    }
    #[cfg(windows)]
    {
        for ext in ["exe", "cmd", "bat", "ps1"] {
            if path.with_extension(ext).is_file() {
                return true;
            }
        }
    }
    false
}

pub async fn resolve_mcp_invocation(driver_cmd: &str) -> (String, Vec<String>) {
    let fallback = (driver_cmd.to_string(), vec![DEFAULT_MCP_ARG.to_string()]);
    let output = match tokio::time::timeout(
        Duration::from_secs(MANIFEST_TIMEOUT_SECS),
        Command::new(driver_cmd)
            .arg("manifest")
            .stdin(Stdio::null())
            .suppress_windows_console()
            .output(),
    )
    .await
    {
        Ok(Ok(output)) if output.status.success() && !output.stdout.is_empty() => output,
        _ => return fallback,
    };
    let Ok(manifest) = serde_json::from_slice::<Value>(&output.stdout) else {
        return fallback;
    };
    let Some(invocation) = manifest.get("mcp_invocation").and_then(Value::as_object) else {
        return fallback;
    };
    let args = invocation
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| vec![DEFAULT_MCP_ARG.to_string()]);
    let command = invocation
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(driver_cmd)
        .to_string();
    (command, args)
}

async fn write_json_line(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
) -> Result<(), ToolError> {
    let mut line = serde_json::to_vec(&value)
        .map_err(|err| ToolError::ExecutionFailed(format!("serialize MCP request: {err}")))?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|err| ToolError::ExecutionFailed(format!("write MCP request: {err}")))?;
    stdin
        .flush()
        .await
        .map_err(|err| ToolError::ExecutionFailed(format!("flush MCP request: {err}")))?;
    Ok(())
}

async fn read_json_response(
    stdout: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: i64,
) -> Result<Value, ToolError> {
    while let Some(line) = stdout
        .next_line()
        .await
        .map_err(|err| ToolError::ExecutionFailed(format!("read MCP response: {err}")))?
    {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|err| {
            ToolError::ExecutionFailed(format!(
                "cua-driver emitted non-JSON MCP line: {err}; raw={}",
                line.chars().take(200).collect::<String>()
            ))
        })?;
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return Ok(value);
        }
    }
    Err(ToolError::ExecutionFailed(format!(
        "cua-driver closed stdout before MCP response id {id}"
    )))
}

fn stderr_tail(stderr: &str) -> String {
    let mut lines: Vec<&str> = stderr.lines().rev().take(3).collect();
    lines.reverse();
    if lines.is_empty() {
        "(empty stderr)".into()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_available_accepts_existing_absolute_path() {
        let current = std::env::current_exe().expect("current exe");
        assert!(command_available(&current.to_string_lossy()));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn resolves_manifest_invocation_and_calls_mcp_tool() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let script = tmp.path().join("fake-cua-driver");
        let log = tmp.path().join("stdin.log");
        let body = format!(
            r#"#!/bin/sh
if [ "$1" = "manifest" ]; then
  printf '%s\n' '{{"mcp_invocation":{{"args":["serve-stdio"],"command":""}}}}'
  exit 0
fi
if [ "$1" = "serve-stdio" ]; then
  while IFS= read -r line; do
    printf '%s\n' "$line" >> "{}"
    case "$line" in
      *'"id":1'*|*'"id": 1'*)
        printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2024-11-05"}}}}'
        ;;
      *'"tools/call"'*)
        printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"structuredContent":{{"ok":true,"called":true}},"content":[{{"type":"text","text":"done"}}]}}}}'
        exit 0
        ;;
    esac
  done
fi
exit 64
"#,
            log.display()
        );
        {
            let mut file = std::fs::File::create(&script).expect("create script");
            file.write_all(body.as_bytes()).expect("write script");
        }
        let mut perms = std::fs::metadata(&script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod");

        let (command, args) = resolve_mcp_invocation(&script.to_string_lossy()).await;
        assert_eq!(std::path::PathBuf::from(&command), script);
        assert_eq!(args, vec!["serve-stdio".to_string()]);

        let backend = CuaDriverBackend::new(command, args);
        let result = backend
            .call_tool("type_text", json!({"text": "hello"}))
            .await
            .expect("call");
        assert_eq!(result["structuredContent"]["called"], true);
        let logged = std::fs::read_to_string(log).expect("read log");
        assert!(
            logged.contains("\"name\":\"type_text\"") || logged.contains("\"name\": \"type_text\"")
        );
        assert!(logged.contains("\"text\":\"hello\"") || logged.contains("\"text\": \"hello\""));
    }
}
