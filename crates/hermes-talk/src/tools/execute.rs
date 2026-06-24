use std::collections::HashSet;
use std::time::Duration;

use tokio::process::Command;
use tracing::{info, warn};

use crate::error::{DemoError, Result};

fn shell_split(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            '\'' => {
                chars.next();
                loop {
                    match chars.next() {
                        None => break,
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                    }
                }
                tokens.push(std::mem::take(&mut current));
            }
            '"' => {
                chars.next();
                loop {
                    match chars.next() {
                        None => break,
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(esc) = chars.next() {
                                current.push(esc);
                            }
                        }
                        Some(ch) => current.push(ch),
                    }
                }
                tokens.push(std::mem::take(&mut current));
            }
            _ => {
                current.push(chars.next().unwrap());
            }
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

pub fn validate_command(cmd_line: &str, allowlist: &[String]) -> Result<Vec<String>> {
    let trimmed = cmd_line.trim();
    if trimmed.is_empty() {
        return Err(DemoError::Tool("empty command".to_string()));
    }

    let tokens = shell_split(trimmed);
    let base = tokens.first().unwrap().as_str();

    let allowed: HashSet<&str> = allowlist.iter().map(|s| s.as_str()).collect();
    if !allowed.contains(base) {
        return Err(DemoError::Tool(format!(
            "command '{}' not in allowlist. allowed: {}",
            base,
            allowlist.join(", ")
        )));
    }

    for t in &tokens {
        let stripped = t.trim_matches(['\'', '"']);
        if stripped.contains("&&") || stripped.contains(';') {
            return Err(DemoError::Tool(
                "chaining operators (&&, ;) not allowed".to_string(),
            ));
        }
        // Allow | inside powershell/cmd invocations
        if base != "powershell" && base != "cmd" && stripped.contains('|') {
            return Err(DemoError::Tool(
                "pipe operator (|) not allowed for this command".to_string(),
            ));
        }
    }

    Ok(tokens)
}

fn rebuild_args(prog: &str, raw: &[String]) -> Vec<String> {
    if prog == "powershell" || prog == "cmd" {
        let mut args: Vec<String> = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let t = &raw[i];
            if t == "-Command" || t.eq_ignore_ascii_case("/c") {
                args.push(t.clone());
                i += 1;
                if i < raw.len() {
                    let rest: String = raw[i..].iter().cloned().collect::<Vec<_>>().join(" ");
                    args.push(rest);
                }
                break;
            }
            args.push(t.clone());
            i += 1;
        }
        args
    } else {
        raw.to_vec()
    }
}

pub async fn run_command(cmd_tokens: Vec<String>) -> Result<String> {
    let prog = cmd_tokens.first().unwrap().as_str();
    let args = rebuild_args(prog, &cmd_tokens[1..]);

    info!(command = %cmd_tokens.join(" "), "execute: running");

    let mut cmd = Command::new(prog);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for a in &args {
        cmd.arg(a);
    }

    let child = cmd
        .spawn()
        .map_err(|e| DemoError::Tool(format!("spawn failed: {e}")))?;

    let result = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

            let mut combined = String::new();
            if !stdout.is_empty() {
                combined.push_str("stdout:\n");
                combined.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str("stderr:\n");
                combined.push_str(&stderr);
            }
            if combined.is_empty() {
                combined.push_str("(no output)");
            }

            if combined.len() > 4096 {
                combined.truncate(4096);
                combined.push_str("\n... (truncated)");
            }

            let exit = output.status.code().unwrap_or(-1);
            info!(exit_code = exit, len = combined.len(), "execute: done");
            Ok(format!("exit_code: {}\n{}", exit, combined.trim()))
        }
        Ok(Err(e)) => {
            warn!(error = %e, "execute: wait failed");
            Ok(format!("error: {e}"))
        }
        Err(_elapsed) => {
            warn!("execute: timeout (>5s)");
            Err(DemoError::Tool("command timed out (>5s)".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowlist_allows() {
        let allowlist: Vec<String> = vec!["date".into(), "uptime".into(), "whoami".into()];
        assert!(validate_command("date", &allowlist).is_ok());
        assert!(validate_command("whoami", &allowlist).is_ok());
        assert!(validate_command("date +%Y", &allowlist).is_ok());
    }

    #[test]
    fn test_allowlist_rejects() {
        let allowlist: Vec<String> = vec!["date".into()];
        assert!(validate_command("rm -rf /", &allowlist).is_err());
        assert!(validate_command("mv a b", &allowlist).is_err());
        assert!(validate_command("date && rm", &allowlist).is_err());
    }

    #[test]
    fn test_no_chaining() {
        let allowlist: Vec<String> = vec!["date".into(), "echo".into()];
        assert!(validate_command("echo a; echo b", &allowlist).is_err());
        assert!(validate_command("date | grep", &allowlist).is_err());
    }

    #[test]
    fn test_powershell_allows_pipe() {
        let allowlist: Vec<String> = vec!["powershell".into(), "cmd".into()];
        assert!(
            validate_command(
                "powershell -Command \"Get-Process | Select-Object Name\"",
                &allowlist,
            )
            .is_ok()
        );
        assert!(validate_command("cmd /c \"dir | findstr test\"", &allowlist).is_ok());
    }

    #[test]
    fn test_powershell_blocks_semicolon() {
        let allowlist: Vec<String> = vec!["powershell".into()];
        assert!(validate_command("powershell -Command a; echo b", &allowlist).is_err());
    }

    #[test]
    fn test_shell_split_plain() {
        let tokens = shell_split("echo hello world");
        assert_eq!(tokens, vec!["echo", "hello", "world"]);
    }

    #[test]
    fn test_shell_split_single_quotes() {
        let tokens = shell_split("date '+%Y-%m-%d %H:%M:%S'");
        assert_eq!(tokens, vec!["date", "+%Y-%m-%d %H:%M:%S"]);
    }

    #[test]
    fn test_shell_split_double_quotes() {
        let tokens = shell_split("date \"+%Y年%m月%d日 %H:%M:%S\"");
        assert_eq!(tokens, vec!["date", "+%Y年%m月%d日 %H:%M:%S"]);
    }

    #[test]
    fn test_shell_split_empty() {
        let tokens = shell_split("  ");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_shell_split_empty_quotes() {
        let tokens = shell_split("echo '' \"\"");
        assert_eq!(tokens, vec!["echo", "", ""]);
    }

    #[test]
    fn test_validate_quoted_date() {
        let allowlist: Vec<String> = vec!["date".into()];
        assert!(validate_command("date '+%Y年%m月%d日 %H:%M:%S'", &allowlist).is_ok());
        assert!(validate_command("date \"+%Y-%m-%d %H:%M:%S\"", &allowlist).is_ok());
    }

    #[test]
    fn test_rebuild_args_quoted() {
        let args = rebuild_args("date", &["+%Y年%m月%d日 %H:%M:%S".into()]);
        assert_eq!(args, vec!["+%Y年%m月%d日 %H:%M:%S"]);
    }

    #[test]
    fn test_rebuild_args_plain() {
        let args = rebuild_args("date", &["+%Y".into()]);
        assert_eq!(args, vec!["+%Y"]);
    }
}
