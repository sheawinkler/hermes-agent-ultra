//! Status, dashboard, debug, and logs CLI command handlers.

use std::path::PathBuf;

use hermes_cli::cli::Cli;
use hermes_config::{
    PlatformConfig, load_config, load_user_config_file, save_config_yaml, validate_config,
};
use hermes_core::AgentError;
use hermes_tools::{default_tool_policy_counters_path, load_tool_policy_counters};

use hermes_cli::paths::CliStateRoot;

use crate::doctor::{
    best_effort_sweep_expired_pending_pastes, collect_debug_report, prune_old_debug_reports,
    record_pending_paste,
};
use hermes_cli::state_paths::hermes_state_root;

pub(crate) async fn run_status(cli: Cli) -> Result<(), AgentError> {
    println!("Hermes Agent Ultra — Status");
    println!("=====================\n");

    println!("Version: {}", env!("CARGO_PKG_VERSION"));

    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    println!(
        "Model:   {}",
        config.model.as_deref().unwrap_or("(default: gpt-4o)")
    );
    println!(
        "Personality: {}",
        config.personality.as_deref().unwrap_or("(none)")
    );
    println!("Max turns: {}", config.max_turns);

    let enabled_platforms: Vec<&String> = config
        .platforms
        .iter()
        .filter(|(_, pc)| pc.enabled)
        .map(|(name, _)| name)
        .collect();
    if enabled_platforms.is_empty() {
        println!("Platforms: (none enabled)");
    } else {
        println!(
            "Platforms: {}",
            enabled_platforms
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let config_dir = hermes_config::hermes_home();
    println!("\nConfig dir: {}", config_dir.display());

    let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "enforce".to_string());
    let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "relaxed".to_string());
    let policy_counters_path = default_tool_policy_counters_path();
    let policy_counters = load_tool_policy_counters(&policy_counters_path).unwrap_or_default();
    println!(
        "Tool policy: mode={} preset={} counters(allow={}, deny={}, audit={}, simulate={}, would_block={})",
        policy_mode,
        policy_preset,
        policy_counters.allow,
        policy_counters.deny,
        policy_counters.audit_only,
        policy_counters.simulate,
        policy_counters.would_block,
    );

    let route_health_path =
        CliStateRoot::from_state_root(&hermes_state_root(&cli)).route_health_state();
    if route_health_path.exists() {
        match std::fs::read_to_string(&route_health_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        {
            Some(v) => {
                let summary = v.get("summary").cloned().unwrap_or_default();
                println!(
                    "Route health: overall={} entries={} avg_score={:.3}",
                    summary
                        .get("overall")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown"),
                    summary.get("entries").and_then(|x| x.as_u64()).unwrap_or(0),
                    summary
                        .get("average_score")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0),
                );
            }
            None => {
                println!(
                    "Route health: unavailable (failed to parse {})",
                    route_health_path.display()
                );
            }
        }
    } else {
        println!("Route health: (not generated) run `hermes route-health` to compute.");
    }

    let sessions_dir = config_dir.join("sessions");
    if sessions_dir.exists() {
        let count = std::fs::read_dir(&sessions_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        println!("Saved sessions: {}", count);
    }

    let profiles_dir = config_dir.join("profiles");
    if profiles_dir.exists() {
        let profiles: Vec<String> = std::fs::read_dir(&profiles_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "yaml" || ext == "yml")
                            .unwrap_or(false)
                    })
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();
        if profiles.is_empty() {
            println!("Profiles: (none)");
        } else {
            println!("Profiles: {}", profiles.join(", "));
        }
    }

    Ok(())
}

pub(crate) fn try_open_url(url: &str) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    cmd.arg(url);

    let status = cmd
        .status()
        .map_err(|e| AgentError::Io(format!("open browser command failed: {}", e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(AgentError::Io(format!(
            "open browser command exited with status {}",
            status
        )))
    }
}

pub(crate) async fn run_dashboard(
    cli: Cli,
    host: String,
    port: u16,
    no_open: bool,
    insecure: bool,
) -> Result<(), AgentError> {
    let host_trimmed = host.trim().to_string();
    let local_host = matches!(host_trimmed.as_str(), "127.0.0.1" | "localhost" | "::1");
    if !local_host && !insecure {
        return Err(AgentError::Config(
            "dashboard refused non-localhost bind without --insecure".into(),
        ));
    }

    let cfg_path = hermes_state_root(&cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    let api = disk
        .platforms
        .entry("api_server".to_string())
        .or_insert_with(PlatformConfig::default);
    api.enabled = true;
    api.extra.insert(
        "host".to_string(),
        serde_json::Value::String(host_trimmed.clone()),
    );
    api.extra
        .insert("port".to_string(), serde_json::Value::Number(port.into()));
    validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
    save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;

    let display_host = if host_trimmed == "0.0.0.0" {
        "127.0.0.1"
    } else {
        host_trimmed.as_str()
    };
    let url = format!("http://{}:{}/", display_host, port);
    println!(
        "Dashboard config written to {} (api_server enabled).",
        cfg_path.display()
    );
    println!("Dashboard URL: {}", url);

    if !no_open {
        let url_for_open = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            if let Err(e) = try_open_url(&url_for_open) {
                eprintln!("Dashboard auto-open failed: {}", e);
            }
        });
    }

    hermes_cli::gateway_runtime::run_gateway(
        cli,
        Some("run".to_string()),
        false,
        false,
        false,
        None,
        false,
        false,
        false,
        false,
    )
    .await
}

pub(crate) async fn run_debug(
    cli: Cli,
    action: Option<String>,
    url: Option<String>,
    lines: u32,
    expire: u32,
    local: bool,
) -> Result<(), AgentError> {
    let reports_dir =
        CliStateRoot::from_config_dir(cli.config_dir.as_deref().map(std::path::Path::new))
            .debug_reports_dir();
    std::fs::create_dir_all(&reports_dir)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", reports_dir.display(), e)))?;
    let now_unix = chrono::Utc::now().timestamp();
    let pending_removed = best_effort_sweep_expired_pending_pastes(&reports_dir, now_unix);
    if pending_removed > 0 {
        println!(
            "Pruned {} expired pending paste record(s).",
            pending_removed
        );
    }
    let removed = prune_old_debug_reports(&reports_dir, expire)?;
    if removed > 0 {
        println!(
            "Pruned {} expired debug report(s) older than {} day(s).",
            removed, expire
        );
    }

    match action.as_deref().unwrap_or("share") {
        "share" => {
            let report = collect_debug_report(&cli, lines)?;
            let filename = format!(
                "{}-debug-report.md",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
            );
            let path = reports_dir.join(filename);
            std::fs::write(&path, &report)
                .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
            println!("Debug report saved: {}", path.display());
            if local {
                println!("{}", report);
                return Ok(());
            }

            match reqwest::Client::new()
                .post("https://paste.rs")
                .header("Content-Type", "text/plain; charset=utf-8")
                .body(report)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let body = resp.text().await.unwrap_or_default();
                    println!("Shared debug report URL: {}", body.trim());
                    let _ = record_pending_paste(
                        &reports_dir,
                        body.trim(),
                        expire,
                        chrono::Utc::now().timestamp(),
                    );
                }
                Ok(resp) => {
                    println!(
                        "Debug share upload failed with status {}. Local report kept at {}",
                        resp.status(),
                        path.display()
                    );
                }
                Err(e) => {
                    println!(
                        "Debug share upload failed: {}. Local report kept at {}",
                        e,
                        path.display()
                    );
                }
            }
        }
        "delete" => {
            let target = url.ok_or_else(|| {
                AgentError::Config(
                    "debug delete requires a local report path or file:// URL".into(),
                )
            })?;
            let path = if let Some(rest) = target.strip_prefix("file://") {
                PathBuf::from(rest)
            } else {
                PathBuf::from(&target)
            };
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
                println!("Removed debug report {}", path.display());
            } else {
                println!("Debug report not found: {}", path.display());
            }
        }
        "list" => {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&reports_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.is_file())
                        .collect()
                })
                .unwrap_or_default();
            entries.sort();
            if entries.is_empty() {
                println!("No debug reports in {}", reports_dir.display());
            } else {
                println!("Debug reports ({}):", reports_dir.display());
                for p in entries {
                    println!("  {}", p.display());
                }
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown debug action '{}'. Use share|delete|list",
                other
            )));
        }
    }
    Ok(())
}

pub(crate) async fn run_logs(cli: Cli, lines: u32, follow: bool) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let log_file = config_dir.join("logs").join("hermes.log");

    if !log_file.exists() {
        println!("No log file found at: {}", log_file.display());
        println!("Logs are written here during interactive sessions.");
        return Ok(());
    }

    if follow {
        println!("Tailing {}... (Ctrl+C to stop)\n", log_file.display());
        let mut child = tokio::process::Command::new("tail")
            .args(["-f", "-n", &lines.to_string()])
            .arg(&log_file)
            .spawn()
            .map_err(|e| AgentError::Io(format!("Failed to tail log file: {}", e)))?;

        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(s) if !s.success() => {
                        eprintln!("tail exited with status: {}", s);
                    }
                    Err(e) => {
                        eprintln!("Error waiting for tail: {}", e);
                    }
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                child.kill().await.ok();
                println!("\nStopped tailing logs.");
            }
        }
    } else {
        let content = std::fs::read_to_string(&log_file)
            .map_err(|e| AgentError::Io(format!("Failed to read log file: {}", e)))?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start = all_lines.len().saturating_sub(lines as usize);
        for line in &all_lines[start..] {
            println!("{}", line);
        }
        println!(
            "\n(Showing last {} of {} lines from {})",
            all_lines.len() - start,
            all_lines.len(),
            log_file.display()
        );
    }
    Ok(())
}
