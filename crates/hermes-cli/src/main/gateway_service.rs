/// Config/state root shared by CLI, `hermes gateway`, cron, and `webhooks.json`.
fn hermes_state_root(cli: &Cli) -> PathBuf {
    state_dir(cli.config_dir.as_deref().map(Path::new))
}

fn gateway_pid_path_for_cli(cli: &Cli) -> PathBuf {
    gateway_pid_path_in(hermes_state_root(cli))
}

const ROUTE_AUTOTUNE_ENV_KEYS: &[&str] = &[
    "HERMES_SMART_ROUTING_LEARNING_ALPHA",
    "HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS",
    "HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN",
    "HERMES_SMART_ROUTING_LEARNING_TTL_SECS",
    "HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS",
];

fn route_autotune_env_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-autotune.env")
}

fn parse_simple_env_file(path: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, value)) = body.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        out.insert(key.to_string(), value);
    }
    out
}

fn apply_route_autotune_env_overrides(cli: &Cli) -> Vec<String> {
    let path = route_autotune_env_path_for_cli(cli);
    if !path.exists() {
        return Vec::new();
    }
    let parsed = parse_simple_env_file(&path);
    let mut applied = Vec::new();
    for key in ROUTE_AUTOTUNE_ENV_KEYS {
        if std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some()
        {
            continue;
        }
        if let Some(value) = parsed.get(*key) {
            std::env::set_var(key, value);
            applied.push((*key).to_string());
        }
    }
    applied
}

fn gateway_lock_path_for_pid_path(pid_path: &Path) -> PathBuf {
    pid_path.with_file_name("gateway.lock")
}
// Discord ingress may take up to 30 seconds to durably drain accepted work.
// Keep a small margin for the remaining gateway teardown before reporting a
// truthful timeout to the operator.
const GATEWAY_STOP_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);
const GATEWAY_STOP_WAIT_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayPidState {
    Dead,
    Gateway,
    LiveUnowned,
    Unverifiable,
}

fn read_gateway_pid(path: &Path) -> Option<u32> {
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

fn read_gateway_pid_or_lock(pid_path: &Path) -> Option<u32> {
    read_gateway_pid(pid_path)
        .or_else(|| read_gateway_pid(&gateway_lock_path_for_pid_path(pid_path)))
}

fn read_gateway_metadata_pids(pid_path: &Path) -> Result<Vec<u32>, AgentError> {
    let lock_path = gateway_lock_path_for_pid_path(pid_path);
    let mut pids = Vec::with_capacity(2);
    for metadata_path in [pid_path, lock_path.as_path()] {
        if !metadata_path.exists() {
            continue;
        }
        let pid = read_gateway_pid(metadata_path).ok_or_else(|| {
            AgentError::Gateway(format!(
                "cannot parse gateway PID metadata at {}; refusing lifecycle action until it is inspected",
                metadata_path.display()
            ))
        })?;
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    Ok(pids)
}


fn cleanup_gateway_metadata_for_pid(pid_path: &Path, pid: u32) {
    if read_gateway_pid(pid_path) == Some(pid) {
        let _ = std::fs::remove_file(pid_path);
    }

    let lock_path = gateway_lock_path_for_pid_path(pid_path);
    if read_gateway_pid(&lock_path) == Some(pid) {
        let _ = std::fs::remove_file(lock_path);
    }
}

fn looks_like_gateway_process(cmdline: &str) -> bool {
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
fn gateway_pid_is_zombie(pid: u32) -> bool {
    let output = match std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    String::from_utf8_lossy(&output.stdout)
        .trim_start()
        .starts_with('Z')
}
#[cfg(unix)]
fn gateway_pid_state(pid: u32) -> GatewayPidState {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            GatewayPidState::Dead
        } else {
            GatewayPidState::Unverifiable
        };
    }
    if gateway_pid_is_zombie(pid) {
        return GatewayPidState::Dead;
    }


    match gateway_pid_commandline(pid) {
        Some(cmdline) if looks_like_gateway_process(&cmdline) => GatewayPidState::Gateway,
        Some(_) => GatewayPidState::LiveUnowned,
        None => GatewayPidState::Unverifiable,
    }
}

#[cfg(not(unix))]
fn gateway_pid_state(_pid: u32) -> GatewayPidState {
    GatewayPidState::Unverifiable
}

fn gateway_pid_is_alive(pid: u32) -> bool {
    !matches!(gateway_pid_state(pid), GatewayPidState::Dead)
}

#[cfg(unix)]
fn gateway_pid_terminate(pid: u32) -> std::io::Result<()> {
    let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn gateway_pid_terminate(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "gateway stop is not supported on this platform",
    ))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayStopOutcome {
    AlreadyExited,
    Stopped,
}

fn validate_gateway_pid_for_stop(pid: u32) -> Result<GatewayStopOutcome, AgentError> {
    match gateway_pid_state(pid) {
        GatewayPidState::Dead => Ok(GatewayStopOutcome::AlreadyExited),
        GatewayPidState::Gateway => Ok(GatewayStopOutcome::Stopped),
        GatewayPidState::LiveUnowned => Err(AgentError::Gateway(format!(
            "refusing to signal PID {pid}: it is alive but does not identify as a Hermes gateway; PID/lock metadata was preserved"
        ))),
        GatewayPidState::Unverifiable => Err(AgentError::Gateway(format!(
            "refusing to signal PID {pid}: its gateway identity could not be verified; PID/lock metadata was preserved"
        ))),
    }
}
fn ensure_gateway_start_slot_available(pid_path: &Path) -> Result<(), AgentError> {
    let lock_path = gateway_lock_path_for_pid_path(pid_path);
    for metadata_path in [pid_path, lock_path.as_path()] {
        if !metadata_path.exists() {
            continue;
        }
        let pid = read_gateway_pid(metadata_path).ok_or_else(|| {
            AgentError::Gateway(format!(
                "cannot parse gateway PID metadata at {}; refusing to start another gateway until it is inspected",
                metadata_path.display()
            ))
        })?;

        match gateway_pid_state(pid) {
            GatewayPidState::Dead => {
                let _ = std::fs::remove_file(metadata_path);
            }
            GatewayPidState::Gateway => {
                return Err(AgentError::Gateway(format!(
                    "gateway PID {pid} is still running or draining; refusing to start another gateway. Wait for `hermes gateway stop` to finish or inspect {}",
                    metadata_path.display()
                )));
            }
            GatewayPidState::LiveUnowned => {
                return Err(AgentError::Gateway(format!(
                    "gateway PID metadata at {} points to live non-gateway PID {pid}; refusing to overwrite it. Verify the process before removing the PID/lock metadata.",
                    metadata_path.display()
                )));
            }
            GatewayPidState::Unverifiable => {
                return Err(AgentError::Gateway(format!(
                    "cannot verify gateway PID {pid} from {}; refusing to start another gateway while PID/lock metadata is retained",
                    metadata_path.display()
                )));
            }
        }
    }
    Ok(())
}


async fn wait_for_gateway_pid_exit(
    pid: u32,
    timeout: std::time::Duration,
) -> Result<(), AgentError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !gateway_pid_is_alive(pid) {
            return Ok(());
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(AgentError::Timeout(format!(
                "gateway PID {pid} is still alive after {} ms following SIGTERM; it may still be draining. PID/lock metadata was preserved. Check `hermes gateway status` and retry `hermes gateway stop` after the drain completes.",
                timeout.as_millis()
            )));
        }

        tokio::time::sleep(std::cmp::min(
            GATEWAY_STOP_WAIT_POLL_INTERVAL,
            deadline - now,
        ))
        .await;
    }
}

async fn stop_gateway_pid_with_timeout(
    pid_path: &Path,
    pid: u32,
    timeout: std::time::Duration,
) -> Result<GatewayStopOutcome, AgentError> {
    match validate_gateway_pid_for_stop(pid)? {
        GatewayStopOutcome::AlreadyExited => {
            cleanup_gateway_metadata_for_pid(pid_path, pid);
            return Ok(GatewayStopOutcome::AlreadyExited);
        }
        GatewayStopOutcome::Stopped => {}
    }

    gateway_pid_terminate(pid)
        .map_err(|error| AgentError::Io(format!("failed to signal gateway PID {pid}: {error}")))?;
    wait_for_gateway_pid_exit(pid, timeout).await?;
    cleanup_gateway_metadata_for_pid(pid_path, pid);
    Ok(GatewayStopOutcome::Stopped)
}

async fn stop_gateway_pid_gracefully(
    pid_path: &Path,
    pid: u32,
) -> Result<GatewayStopOutcome, AgentError> {
    stop_gateway_pid_with_timeout(pid_path, pid, GATEWAY_STOP_WAIT_TIMEOUT).await
}

#[cfg(target_os = "macos")]
fn gateway_launchd_label() -> &'static str {
    "com.hermes_agent_ultra.gateway"
}

#[cfg(target_os = "macos")]
fn gateway_launchd_plist_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", gateway_launchd_label())),
    )
}

#[cfg(target_os = "macos")]
fn launchd_target() -> String {
    let uid = unsafe { libc::geteuid() };
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
fn launchctl_bootstrap(plist: &Path) -> Result<(), AgentError> {
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

fn install_gateway_service(force: bool, dry_run: bool) -> Result<(), AgentError> {
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

fn uninstall_gateway_service(dry_run: bool) -> Result<(), AgentError> {
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

fn try_start_gateway_service() -> Result<bool, AgentError> {
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

fn try_stop_gateway_service() -> Result<bool, AgentError> {
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

fn try_restart_gateway_service() -> Result<bool, AgentError> {
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

fn gateway_service_status() -> Result<Option<String>, AgentError> {
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

fn migrate_legacy_gateway_services(dry_run: bool, yes: bool) -> Result<(), AgentError> {
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

