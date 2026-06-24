//! Gateway process management — PID file read/write, process liveness,
//! launchd service install/uninstall, and legacy migration.
//!
//! Extracted from the monolithic `main.rs` binary entry point.

use hermes_core::AgentError;

#[cfg(target_os = "macos")]
use hermes_config::hermes_home;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// PID file helpers
// ---------------------------------------------------------------------------

pub(crate) fn gateway_lock_path_for_pid_path(pid_path: &Path) -> PathBuf {
    pid_path.with_file_name("gateway.lock")
}

pub fn read_gateway_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(pid) = trimmed.parse::<u32>() {
        return Some(pid);
    }
    let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pid = json.get("pid")?.as_u64()?;
    u32::try_from(pid).ok()
}

pub(crate) fn cleanup_stale_gateway_metadata(pid_path: &Path) {
    let _ = std::fs::remove_file(pid_path);
    let _ = std::fs::remove_file(gateway_lock_path_for_pid_path(pid_path));
}

#[cfg(unix)]
pub(crate) fn looks_like_gateway_process(cmdline: &str) -> bool {
    let cmdline = cmdline.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "hermes_cli.main gateway",
        "hermes_cli/main.py gateway",
        "hermes gateway",
        "hermes-agent-ultra gateway",
        "hermes-gateway",
        "gateway/run.py",
    ];
    PATTERNS.iter().any(|pattern| cmdline.contains(pattern))
}

// ---------------------------------------------------------------------------
// Unix process inspection
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn gateway_pid_commandline(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let cmdline = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cmdline.is_empty() {
        None
    } else {
        Some(cmdline)
    }
}

#[cfg(unix)]
pub fn gateway_pid_is_alive(pid: u32) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, 0) != 0 } {
        return false;
    }
    match gateway_pid_commandline(pid) {
        Some(cmdline) => looks_like_gateway_process(&cmdline),
        None => true,
    }
}

#[cfg(not(unix))]
pub fn gateway_pid_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
pub(crate) fn gateway_pid_terminate(pid: u32) -> std::io::Result<()> {
    let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub(crate) fn gateway_pid_terminate(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "gateway stop is not supported on this platform",
    ))
}

// ---------------------------------------------------------------------------
// launchd service (macOS only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub(crate) fn gateway_launchd_label() -> &'static str {
    "com.hermes_agent_ultra.gateway"
}

#[cfg(target_os = "macos")]
pub(crate) fn gateway_launchd_plist_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", gateway_launchd_label())),
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn launchd_target() -> String {
    let uid = unsafe { libc::geteuid() };
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
pub(crate) fn launchctl_bootstrap(plist: &Path) -> Result<(), AgentError> {
    let target = launchd_target();
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &target])
        .arg(plist)
        .status();
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &target])
        .arg(plist)
        .status()
        .map_err(|e| AgentError::Io(format!("launchctl bootstrap: {e}")))?;
    if !status.success() {
        return Err(AgentError::Io(format!(
            "launchctl bootstrap failed for {}",
            plist.display()
        )));
    }
    let label = format!("{target}/{}", gateway_launchd_label());
    let _ = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &label])
        .status();
    Ok(())
}

// ---------------------------------------------------------------------------
// Service install / uninstall / start / stop / restart / status
// ---------------------------------------------------------------------------

pub(crate) fn install_gateway_service(force: bool, dry_run: bool) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Err(AgentError::Io(
                "unable to resolve launchd plist path".into(),
            ));
        };
        if plist_path.exists() && !force {
            println!(
                "Gateway service already installed at {} (use --force to overwrite).",
                plist_path.display()
            );
            return Ok(());
        }
        let agents_dir = plist_path
            .parent()
            .ok_or_else(|| AgentError::Io("invalid launch agents path".into()))?;
        if dry_run {
            println!(
                "Dry-run: would install gateway service plist at {}",
                plist_path.display()
            );
            return Ok(());
        }
        std::fs::create_dir_all(agents_dir)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {e}", agents_dir.display())))?;
        let exe = std::env::current_exe()
            .map_err(|e| AgentError::Io(format!("current_exe failed: {e}")))?;
        let logs_dir = hermes_home().join("logs");
        std::fs::create_dir_all(&logs_dir)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {e}", logs_dir.display())))?;
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{exe}</string>
      <string>gateway</string>
      <string>run</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{stdout}</string>
    <key>StandardErrorPath</key><string>{stderr}</string>
  </dict>
</plist>
"#,
            label = gateway_launchd_label(),
            exe = exe.display(),
            stdout = logs_dir.join("gateway-service.log").display(),
            stderr = logs_dir.join("gateway-service.err.log").display(),
        );
        std::fs::write(&plist_path, plist)
            .map_err(|e| AgentError::Io(format!("write {}: {e}", plist_path.display())))?;
        launchctl_bootstrap(&plist_path)?;
        println!(
            "Installed gateway launchd service at {}",
            plist_path.display()
        );
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (force, dry_run);
        println!("Gateway install is currently implemented for macOS launchd only.");
        Ok(())
    }
}

pub(crate) fn uninstall_gateway_service(dry_run: bool) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Err(AgentError::Io(
                "unable to resolve launchd plist path".into(),
            ));
        };
        if dry_run {
            println!(
                "Dry-run: would uninstall gateway service plist {}",
                plist_path.display()
            );
            return Ok(());
        }
        if plist_path.exists() {
            let target = launchd_target();
            let _ = std::process::Command::new("launchctl")
                .args(["bootout", &target])
                .arg(&plist_path)
                .status();
            std::fs::remove_file(&plist_path)
                .map_err(|e| AgentError::Io(format!("remove {}: {e}", plist_path.display())))?;
            println!("Removed gateway launchd service {}", plist_path.display());
        } else {
            println!("Gateway service is not installed.");
        }
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = dry_run;
        println!("Gateway uninstall is currently implemented for macOS launchd only.");
        Ok(())
    }
}

pub(crate) fn try_start_gateway_service() -> Result<bool, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(false);
        };
        if !plist_path.exists() {
            return Ok(false);
        }
        launchctl_bootstrap(&plist_path)?;
        return Ok(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

pub(crate) fn try_stop_gateway_service() -> Result<bool, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(false);
        };
        if !plist_path.exists() {
            return Ok(false);
        }
        let target = launchd_target();
        let status = std::process::Command::new("launchctl")
            .args(["bootout", &target])
            .arg(plist_path)
            .status()
            .map_err(|e| AgentError::Io(format!("launchctl bootout: {e}")))?;
        return Ok(status.success());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

pub(crate) fn try_restart_gateway_service() -> Result<bool, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(false);
        };
        if !plist_path.exists() {
            return Ok(false);
        }
        launchctl_bootstrap(&plist_path)?;
        return Ok(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

pub fn gateway_service_status() -> Result<Option<String>, AgentError> {
    #[cfg(target_os = "macos")]
    {
        let Some(plist_path) = gateway_launchd_plist_path() else {
            return Ok(None);
        };
        if !plist_path.exists() {
            return Ok(Some("Gateway service: not installed".to_string()));
        }
        let label = format!("{}/{}", launchd_target(), gateway_launchd_label());
        let out = std::process::Command::new("launchctl")
            .args(["print", &label])
            .output()
            .map_err(|e| AgentError::Io(format!("launchctl print: {e}")))?;
        if out.status.success() {
            return Ok(Some(format!(
                "Gateway service: installed (launchd label {}, running)",
                gateway_launchd_label()
            )));
        }
        Ok(Some(format!(
            "Gateway service: installed (launchd label {}, stopped)",
            gateway_launchd_label()
        )))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

pub(crate) fn migrate_legacy_gateway_services(dry_run: bool, yes: bool) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or_else(|| AgentError::Io("home dir not found".into()))?;
        let agents = home.join("Library").join("LaunchAgents");
        if !agents.exists() {
            println!("No LaunchAgents directory found; nothing to migrate.");
            return Ok(());
        }
        let mut legacy_plists: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&agents)
            .map_err(|e| AgentError::Io(format!("read {}: {e}", agents.display())))?
        {
            let entry = entry.map_err(|e| AgentError::Io(e.to_string()))?;
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let lower = file_name.to_ascii_lowercase();
            if lower.contains("hermes")
                && lower.contains("gateway")
                && file_name != format!("{}.plist", gateway_launchd_label())
            {
                legacy_plists.push(path);
            }
        }
        if legacy_plists.is_empty() {
            println!("No legacy gateway launchd units detected.");
            return Ok(());
        }
        println!("Legacy gateway units detected:");
        for p in &legacy_plists {
            println!("  - {}", p.display());
        }
        if !yes && !dry_run {
            return Err(AgentError::Config(
                "Refusing to remove legacy units without --yes (or use --dry-run).".into(),
            ));
        }
        if dry_run {
            println!("Dry-run complete; no files removed.");
            return Ok(());
        }
        let target = launchd_target();
        for p in legacy_plists {
            let _ = std::process::Command::new("launchctl")
                .args(["bootout", &target])
                .arg(&p)
                .status();
            let _ = std::fs::remove_file(&p);
            println!("Removed {}", p.display());
        }
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dry_run, yes);
        println!("Legacy gateway migration is currently implemented for macOS launchd only.");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_gateway_pid_supports_plain_and_json_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plain = tmp.path().join("plain.pid");
        std::fs::write(&plain, "12345\n").expect("write plain pid");
        assert_eq!(read_gateway_pid(&plain), Some(12345));

        let json = tmp.path().join("json.pid");
        std::fs::write(
            &json,
            serde_json::json!({
                "pid": 23456,
                "kind": "hermes-gateway",
                "argv": ["hermes-gateway"]
            })
            .to_string(),
        )
        .expect("write json pid");
        assert_eq!(read_gateway_pid(&json), Some(23456));

        let invalid = tmp.path().join("invalid.pid");
        std::fs::write(&invalid, "{bad").expect("write invalid pid");
        assert_eq!(read_gateway_pid(&invalid), None);
    }

    #[test]
    fn looks_like_gateway_process_includes_gateway_script_pattern() {
        assert!(looks_like_gateway_process(
            "python -m hermes_cli.main gateway run"
        ));
        assert!(looks_like_gateway_process(
            "python hermes_cli/main.py gateway run"
        ));
        assert!(looks_like_gateway_process("hermes gateway run"));
        assert!(looks_like_gateway_process(
            "hermes-gateway --config ~/.hermes"
        ));
        assert!(looks_like_gateway_process("python gateway/run.py"));
        assert!(!looks_like_gateway_process("python worker.py"));
    }

    #[test]
    fn cleanup_stale_gateway_metadata_removes_pid_and_lock_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid_path = tmp.path().join("gateway.pid");
        let lock_path = gateway_lock_path_for_pid_path(&pid_path);
        std::fs::write(&pid_path, "999999\n").expect("write pid");
        std::fs::write(&lock_path, "{\"pid\":999999}").expect("write lock");

        cleanup_stale_gateway_metadata(&pid_path);
        assert!(!pid_path.exists());
        assert!(!lock_path.exists());
    }
}
