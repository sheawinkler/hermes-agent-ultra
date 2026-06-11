use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::SystemTime;

use hermes_core::AgentError;

pub(crate) fn parse_env_file_kv(path: &Path) -> Vec<(String, String)> {
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let (k, v) = trimmed.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

pub(crate) fn write_autopilot_runtime_event(
    report_dir: &Path,
    session_id: &str,
    mode: &str,
    profile: &str,
    applied: &[(String, String)],
) {
    let path = report_dir.join("performance-autopilot-runtime.jsonl");
    let created_at = format!("{:?}", SystemTime::now());
    let payload = serde_json::json!({
        "created_at": created_at,
        "session_id": session_id,
        "mode": mode,
        "profile": profile,
        "applied": applied,
    });
    if let Ok(line) = serde_json::to_string(&payload) {
        if let Ok(mut fh) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(&mut fh, "{line}");
        }
    }
}

pub(crate) fn dashboard_status_line_from_payload(payload: &serde_json::Value) -> String {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("n/a");
    format!(
        "dashboard: {} ({})",
        if enabled { "ON" } else { "OFF" },
        url
    )
}

pub(crate) async fn run_ops_shell_command(command: &str) -> Result<String, AgentError> {
    let output = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("ops shell command failed: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut msg = String::new();
    if !stdout.is_empty() {
        msg.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !msg.is_empty() {
            msg.push_str("\n\n");
        }
        msg.push_str("stderr:\n");
        msg.push_str(&stderr);
    }
    if msg.is_empty() {
        msg = format!("(exit: {})", output.status);
    } else if !output.status.success() {
        msg = format!("(exit: {})\n{}", output.status, msg);
    }
    Ok(msg)
}

pub(crate) async fn run_current_hermes_cli_command(args: &[&str]) -> Result<String, AgentError> {
    let exe = std::env::current_exe()
        .map_err(|e| AgentError::Io(format!("resolve current executable: {e}")))?;
    let output = tokio::process::Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("run current hermes command failed: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut msg = String::new();
    if !stdout.is_empty() {
        msg.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !msg.is_empty() {
            msg.push_str("\n\n");
        }
        msg.push_str("stderr:\n");
        msg.push_str(&stderr);
    }
    if msg.is_empty() {
        msg = format!("(exit: {})", output.status);
    } else if !output.status.success() {
        msg = format!("(exit: {})\n{}", output.status, msg);
    }
    Ok(msg)
}

pub(crate) fn shell_escape(input: &str) -> String {
    let escaped = input.replace('\'', "'\"'\"'");
    format!("'{}'", escaped)
}
