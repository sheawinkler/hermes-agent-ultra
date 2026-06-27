/// Handle `hermes status`.
async fn run_status(cli: Cli) -> Result<(), AgentError> {
    println!("Hermes Agent Ultra — Status");
    println!("=====================\n");

    println!("Version: {}", env!("CARGO_PKG_VERSION"));

    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    println!(
        "Model:   {}",
        config.model.as_deref().unwrap_or("(default: gpt-5.5)")
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

    let route_health_path = route_health_state_path_for_cli(&cli);
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

    // Check for active sessions
    let sessions_dir = config_dir.join("sessions");
    if sessions_dir.exists() {
        let count = std::fs::read_dir(&sessions_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        println!("Saved sessions: {}", count);
    }

    // Check for profiles
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

/// Handle `hermes logs`.
fn try_open_url(url: &str) -> Result<(), AgentError> {
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

fn debug_reports_dir_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("debug-reports")
}

fn prune_old_debug_reports(path: &Path, expire_days: u32) -> Result<usize, AgentError> {
    if !path.exists() {
        return Ok(0);
    }
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(expire_days as u64 * 86_400))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let mut removed = 0usize;
    for entry in std::fs::read_dir(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?
    {
        let entry = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let modified = std::fs::metadata(&p)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if modified < cutoff {
            if std::fs::remove_file(&p).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

const DEBUG_LOG_SNAPSHOT_MAX_BYTES: usize = 512 * 1024;
const DEBUG_PENDING_PASTES_FILE: &str = "pending-pastes.json";

#[derive(Debug, Clone)]
struct DebugLogSnapshot {
    tail_text: String,
    #[cfg_attr(not(test), allow(dead_code))]
    full_text: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct PendingPasteDelete {
    url: String,
    expires_at_unix: i64,
}

fn debug_pending_pastes_path(reports_dir: &Path) -> PathBuf {
    reports_dir.join(DEBUG_PENDING_PASTES_FILE)
}

fn best_effort_sweep_expired_pending_pastes(reports_dir: &Path, now_unix: i64) -> usize {
    sweep_expired_pending_pastes(reports_dir, now_unix).unwrap_or(0)
}

fn sweep_expired_pending_pastes(reports_dir: &Path, now_unix: i64) -> Result<usize, AgentError> {
    let store = debug_pending_pastes_path(reports_dir);
    if !store.exists() {
        return Ok(0);
    }
    let content = std::fs::read_to_string(&store)
        .map_err(|e| AgentError::Io(format!("read {}: {}", store.display(), e)))?;
    let entries: Vec<PendingPasteDelete> = serde_json::from_str(&content).unwrap_or_default();
    if entries.is_empty() {
        let _ = std::fs::remove_file(&store);
        return Ok(0);
    }

    let mut kept: Vec<PendingPasteDelete> = Vec::new();
    let mut removed = 0usize;
    for entry in entries {
        if entry.expires_at_unix <= now_unix {
            removed += 1;
        } else {
            kept.push(entry);
        }
    }

    if removed == 0 {
        return Ok(0);
    }

    if kept.is_empty() {
        std::fs::remove_file(&store)
            .map_err(|e| AgentError::Io(format!("remove {}: {}", store.display(), e)))?;
    } else {
        let body = serde_json::to_string_pretty(&kept)
            .map_err(|e| AgentError::Config(format!("serialize pending paste store: {}", e)))?;
        std::fs::write(&store, body)
            .map_err(|e| AgentError::Io(format!("write {}: {}", store.display(), e)))?;
    }
    Ok(removed)
}

fn record_pending_paste(
    reports_dir: &Path,
    url: &str,
    expire_days: u32,
    now_unix: i64,
) -> Result<(), AgentError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let store = debug_pending_pastes_path(reports_dir);
    let mut entries: Vec<PendingPasteDelete> = if store.exists() {
        std::fs::read_to_string(&store)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<PendingPasteDelete>>(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let expires_at_unix = now_unix.saturating_add((expire_days as i64).saturating_mul(86_400));
    entries.push(PendingPasteDelete {
        url: trimmed.to_string(),
        expires_at_unix,
    });
    let body = serde_json::to_string_pretty(&entries)
        .map_err(|e| AgentError::Config(format!("serialize pending paste store: {}", e)))?;
    std::fs::write(&store, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", store.display(), e)))?;
    Ok(())
}

fn capture_debug_log_snapshot(
    log_file: &Path,
    tail_lines: usize,
    max_bytes: usize,
) -> DebugLogSnapshot {
    if !log_file.exists() {
        return DebugLogSnapshot {
            tail_text: "(file not found)".to_string(),
            full_text: None,
        };
    }

    let mut raw: Vec<u8> = Vec::new();
    let mut truncated = false;
    let read_result: Result<(), String> = (|| {
        let mut file = std::fs::File::open(log_file)
            .map_err(|e| format!("open {}: {}", log_file.display(), e))?;
        let size = file
            .metadata()
            .map_err(|e| format!("stat {}: {}", log_file.display(), e))?
            .len() as usize;
        if size == 0 {
            return Ok(());
        }

        if size <= max_bytes {
            file.read_to_end(&mut raw)
                .map_err(|e| format!("read {}: {}", log_file.display(), e))?;
            return Ok(());
        }

        let mut pos = size as u64;
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let mut total = 0usize;
        let mut newline_count = 0usize;
        let mut chunk_size = max_bytes.min(8192).max(1);
        let hard_cap = max_bytes.saturating_mul(2).max(max_bytes);

        while pos > 0
            && (total < max_bytes || newline_count < tail_lines.saturating_add(1))
            && total < hard_cap
        {
            let read_size = chunk_size.min(pos as usize);
            pos -= read_size as u64;
            file.seek(SeekFrom::Start(pos))
                .map_err(|e| format!("seek {}: {}", log_file.display(), e))?;
            let mut buf = vec![0u8; read_size];
            file.read_exact(&mut buf)
                .map_err(|e| format!("read {}: {}", log_file.display(), e))?;
            newline_count += buf.iter().filter(|b| **b == b'\n').count();
            total += buf.len();
            chunks.push(buf);
            chunk_size = (chunk_size * 2).min(65_536);
        }

        chunks.reverse();
        raw = chunks.concat();
        truncated = pos > 0;
        Ok(())
    })();

    if let Err(err) = read_result {
        return DebugLogSnapshot {
            tail_text: format!("(error reading: {err})"),
            full_text: None,
        };
    }

    let mut full_raw = raw.clone();
    if truncated && full_raw.len() > max_bytes {
        let cut = full_raw.len() - max_bytes;
        let on_boundary = cut > 0 && full_raw[cut - 1] == b'\n';
        full_raw = full_raw[cut..].to_vec();
        if !on_boundary {
            if let Some(idx) = full_raw.iter().position(|b| *b == b'\n') {
                full_raw = full_raw[idx + 1..].to_vec();
            }
        }
    }

    let text = String::from_utf8_lossy(&raw);
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return DebugLogSnapshot {
            tail_text: "(file empty)".to_string(),
            full_text: None,
        };
    }

    let start = lines.len().saturating_sub(tail_lines);
    let tail = lines.drain(start..).collect::<Vec<_>>().join("\n");
    let mut full_text = String::from_utf8_lossy(&full_raw).to_string();
    if truncated {
        full_text = format!(
            "[... truncated — showing last ~{}KB ...]\n{}",
            max_bytes / 1024,
            full_text
        );
    }
    DebugLogSnapshot {
        tail_text: tail,
        full_text: Some(full_text),
    }
}

fn collect_debug_report(cli: &Cli, lines: u32) -> Result<String, AgentError> {
    let now = chrono::Utc::now().to_rfc3339();
    let root = hermes_state_root(cli);
    let cfg_path = root.join("config.yaml");
    let log_file = root.join("logs").join("hermes.log");
    let mut report = String::new();
    report.push_str("# Hermes Debug Report\n\n");
    report.push_str(&format!("- generated_at: {}\n", now));
    report.push_str(&format!("- version: {}\n", env!("CARGO_PKG_VERSION")));
    report.push_str(&format!("- os: {}\n", std::env::consts::OS));
    report.push_str(&format!("- arch: {}\n", std::env::consts::ARCH));
    report.push_str(&format!("- state_root: {}\n", root.display()));
    report.push_str(&format!("- config_path: {}\n", cfg_path.display()));
    report.push_str(&format!("- log_path: {}\n", log_file.display()));
    if let Some(svc) = gateway_service_status()? {
        report.push_str(&format!(
            "- gateway_service: {}\n",
            svc.replace('\n', " | ")
        ));
    }
    let pid_path = gateway_pid_path_for_cli(cli);
    if let Some(pid) = read_gateway_pid(&pid_path) {
        report.push_str(&format!(
            "- gateway_pid: {} (alive={})\n",
            pid,
            gateway_pid_is_alive(pid)
        ));
    } else {
        report.push_str("- gateway_pid: none\n");
    }
    if let Ok(cfg) = load_config(cli.config_dir.as_deref()) {
        report.push_str("\n## Config Summary\n");
        report.push_str(&format!(
            "- model: {}\n",
            cfg.model.as_deref().unwrap_or("gpt-5.5")
        ));
        report.push_str(&format!(
            "- personality: {}\n",
            cfg.personality.as_deref().unwrap_or("default")
        ));
        let mut enabled_platforms: Vec<String> = cfg
            .platforms
            .iter()
            .filter_map(|(k, v)| v.enabled.then_some(k.clone()))
            .collect();
        enabled_platforms.sort();
        report.push_str(&format!(
            "- enabled_platforms: {}\n",
            if enabled_platforms.is_empty() {
                "(none)".to_string()
            } else {
                enabled_platforms.join(", ")
            }
        ));
    }
    report.push_str("\n## Recent Logs\n\n```\n");
    let snapshot =
        capture_debug_log_snapshot(&log_file, lines as usize, DEBUG_LOG_SNAPSHOT_MAX_BYTES);
    report.push_str(&snapshot.tail_text);
    report.push('\n');
    report.push_str("```\n");
    Ok(report)
}

async fn run_dashboard(
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

    run_gateway(
        cli,
        Some("run".to_string()),
        None,
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

async fn run_debug(
    cli: Cli,
    action: Option<String>,
    url: Option<String>,
    lines: u32,
    expire: u32,
    local: bool,
) -> Result<(), AgentError> {
    let reports_dir = debug_reports_dir_for_cli(&cli);
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

async fn run_logs(cli: Cli, lines: u32, follow: bool) -> Result<(), AgentError> {
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

fn profile_aliases_path(profiles_dir: &Path) -> PathBuf {
    profiles_dir.join("aliases.json")
}

fn active_profile_marker_path(profiles_dir: &Path) -> PathBuf {
    profiles_dir.join(".active_profile")
}

fn load_profile_aliases(
    path: &Path,
) -> Result<std::collections::BTreeMap<String, String>, AgentError> {
    if !path.exists() {
        return Ok(std::collections::BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_profile_aliases(
    path: &Path,
    aliases: &std::collections::BTreeMap<String, String>,
) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw =
        serde_json::to_string_pretty(aliases).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn resolve_profile_name(
    requested: &str,
    aliases: &std::collections::BTreeMap<String, String>,
) -> String {
    aliases
        .get(requested.trim())
        .cloned()
        .unwrap_or_else(|| requested.trim().to_string())
}

fn profile_alias_label(
    aliases: &std::collections::BTreeMap<String, String>,
    target: &str,
) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let mut custom = Vec::new();
    let mut profile_named = Vec::new();
    for (alias, resolved_target) in aliases {
        let alias = alias.trim();
        if alias.is_empty() || resolved_target.trim() != target {
            continue;
        }
        if alias == target {
            profile_named.push(alias.to_string());
        } else {
            custom.push(alias.to_string());
        }
    }
    let selected = if custom.is_empty() {
        profile_named
    } else {
        custom
    };
    match selected.as_slice() {
        [] => None,
        [one] => Some(format!("alias: {one}")),
        many => Some(format!("aliases: {}", many.join(", "))),
    }
}

fn resolve_profile_yaml_path(profiles_dir: &Path, name: &str) -> Option<PathBuf> {
    let yaml = profiles_dir.join(format!("{}.yaml", name));
    if yaml.exists() {
        return Some(yaml);
    }
    let yml = profiles_dir.join(format!("{}.yml", name));
    if yml.exists() {
        return Some(yml);
    }
    None
}

fn read_active_profile_name(profiles_dir: &Path) -> Option<String> {
    std::fs::read_to_string(active_profile_marker_path(profiles_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_active_profile_name(profiles_dir: &Path, name: &str) -> Result<(), AgentError> {
    let path = active_profile_marker_path(profiles_dir);
    std::fs::create_dir_all(profiles_dir)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", profiles_dir.display(), e)))?;
    std::fs::write(&path, format!("{}\n", name.trim()))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn load_profile_yaml(path: &Path) -> Result<serde_yaml::Value, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_yaml::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_profile_yaml(path: &Path, value: &serde_yaml::Value) -> Result<(), AgentError> {
    let raw = serde_yaml::to_string(value).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn validate_profile_name(name: &str) -> Result<String, AgentError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AgentError::Config("profile name cannot be empty".into()));
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains('/') || trimmed.contains('\\') {
        return Err(AgentError::Config(format!(
            "invalid profile name '{}': path separators are not allowed",
            trimmed
        )));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(AgentError::Config(format!(
            "invalid profile name '{}': use letters, numbers, '-', '_' or '.'",
            trimmed
        )));
    }
    Ok(trimmed.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn run_profile(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    secondary: Option<String>,
    output: Option<String>,
    import_name: Option<String>,
    alias_name: Option<String>,
    remove: bool,
    yes: bool,
    clone: bool,
    clone_all: bool,
    clone_from: Option<String>,
    no_alias: bool,
    no_skills: bool,
) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let profiles_dir = config_dir.join("profiles");
    let aliases_path = profile_aliases_path(&profiles_dir);
    let mut aliases = load_profile_aliases(&aliases_path)?;

    match action.as_deref().unwrap_or("show") {
        "show" => {
            if let Some(requested) = name {
                let resolved = resolve_profile_name(&requested, &aliases);
                let Some(path) = resolve_profile_yaml_path(&profiles_dir, &resolved) else {
                    return Err(AgentError::Config(format!(
                        "profile '{}' not found (resolved to '{}')",
                        requested, resolved
                    )));
                };
                let raw = std::fs::read_to_string(&path)
                    .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
                println!("{}", raw);
                return Ok(());
            }
            let config = load_config(cli.config_dir.as_deref())
                .map_err(|e| AgentError::Config(e.to_string()))?;
            let active =
                read_active_profile_name(&profiles_dir).unwrap_or_else(|| "(none)".to_string());
            println!("Current profile:");
            println!("  Active:      {}", active);
            if let Some(label) = profile_alias_label(&aliases, &active) {
                if let Some(alias) = label.strip_prefix("alias: ") {
                    println!("  Alias:       {alias}");
                } else if let Some(alias) = label.strip_prefix("aliases: ") {
                    println!("  Aliases:     {alias}");
                }
            }
            println!(
                "  Model:       {}",
                config.model.as_deref().unwrap_or("gpt-5.5")
            );
            println!(
                "  Personality: {}",
                config.personality.as_deref().unwrap_or("default")
            );
            println!("  Max turns:   {}", config.max_turns);
            println!("\nUse `hermes profile list` to see all profiles.");
        }
        "list" => {
            if !profiles_dir.exists() {
                println!("No profiles directory found. Run `hermes-ultra setup` first.");
                return Ok(());
            }
            let active = read_active_profile_name(&profiles_dir);
            let mut entries: Vec<String> = std::fs::read_dir(&profiles_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
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
            entries.sort();

            if entries.is_empty() {
                println!("No profiles found. Create one with `hermes profile create <name>`.");
            } else {
                println!("Available profiles:");
                for name in &entries {
                    let marker = if active.as_deref() == Some(name.as_str()) {
                        "*"
                    } else {
                        " "
                    };
                    let alias = profile_alias_label(&aliases, name)
                        .map(|label| format!(" ({label})"))
                        .unwrap_or_default();
                    println!("{} {}{}", marker, name, alias);
                }
                if !aliases.is_empty() {
                    println!("\nAliases:");
                    for (alias, target) in &aliases {
                        println!("  {} -> {}", alias, target);
                    }
                }
            }
        }
        "create" => {
            let profile_name = name.ok_or_else(|| {
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile create <name>".into(),
                )
            })?;
            let profile_name = validate_profile_name(&profile_name)?;

            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("Failed to create profiles dir: {}", e)))?;

            let profile_path = profiles_dir.join(format!("{}.yaml", profile_name));
            if profile_path.exists() {
                return Err(AgentError::Config(format!(
                    "Profile '{}' already exists at {}",
                    profile_name,
                    profile_path.display()
                )));
            }

            let explicit_clone_from = clone_from
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some();
            let source_name = clone_from
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| resolve_profile_name(s, &aliases))
                .or_else(|| read_active_profile_name(&profiles_dir));
            let source_value = if clone || clone_all || explicit_clone_from {
                let src = source_name.clone().ok_or_else(|| {
                    AgentError::Config(
                        "profile create --clone/--clone-all/--clone-from requires a source profile"
                            .into(),
                    )
                })?;
                let src_path = resolve_profile_yaml_path(&profiles_dir, &src).ok_or_else(|| {
                    AgentError::Config(format!("clone source profile '{}' not found", src))
                })?;
                Some(load_profile_yaml(&src_path)?)
            } else {
                None
            };

            let mut out_map = serde_yaml::Mapping::new();
            out_map.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(profile_name.clone()),
            );

            if let Some(src) = source_value {
                if let Some(src_map) = src.as_mapping() {
                    if clone_all {
                        out_map = src_map.clone();
                        out_map.insert(
                            serde_yaml::Value::String("name".to_string()),
                            serde_yaml::Value::String(profile_name.clone()),
                        );
                    } else {
                        for key in ["model", "personality", "max_turns"] {
                            let k = serde_yaml::Value::String(key.to_string());
                            if let Some(v) = src_map.get(&k) {
                                out_map.insert(k, v.clone());
                            }
                        }
                    }
                }
            }

            if no_skills {
                let skills_key = serde_yaml::Value::String("skills".to_string());
                let overrides_key = serde_yaml::Value::String("skill_overrides".to_string());
                out_map.remove(&skills_key);
                out_map.remove(&overrides_key);
            }

            out_map
                .entry(serde_yaml::Value::String("model".to_string()))
                .or_insert_with(|| serde_yaml::Value::String("openai:gpt-5.5".to_string()));
            out_map
                .entry(serde_yaml::Value::String("personality".to_string()))
                .or_insert_with(|| serde_yaml::Value::String("default".to_string()));
            out_map
                .entry(serde_yaml::Value::String("max_turns".to_string()))
                .or_insert_with(|| serde_yaml::Value::Number(serde_yaml::Number::from(50u64)));

            save_profile_yaml(&profile_path, &serde_yaml::Value::Mapping(out_map))?;
            println!(
                "Created profile '{}' at {}",
                profile_name,
                profile_path.display()
            );

            if !no_alias {
                if let Some(alias) = alias_name
                    .or(secondary)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    aliases.insert(alias.clone(), profile_name.clone());
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Added alias '{}' -> '{}'.", alias, profile_name);
                }
            }
        }
        "use" | "switch" => {
            let requested = name.ok_or_else(|| {
                AgentError::Config("Missing profile name. Usage: hermes profile use <name>".into())
            })?;
            let resolved = resolve_profile_name(&requested, &aliases);
            let path = resolve_profile_yaml_path(&profiles_dir, &resolved).ok_or_else(|| {
                AgentError::Config(format!(
                    "Profile '{}' not found (resolved to '{}')",
                    requested, resolved
                ))
            })?;
            let value = load_profile_yaml(&path)?;
            let mut disk = load_user_config_file(&config_dir.join("config.yaml"))
                .map_err(|e| AgentError::Config(e.to_string()))?;
            if let Some(map) = value.as_mapping() {
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("model".to_string()))
                    .and_then(|v| v.as_str())
                {
                    disk.model = Some(v.to_string());
                }
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("personality".to_string()))
                    .and_then(|v| v.as_str())
                {
                    disk.personality = Some(v.to_string());
                }
                if let Some(v) = map
                    .get(&serde_yaml::Value::String("max_turns".to_string()))
                    .and_then(|v| v.as_u64())
                {
                    disk.max_turns = v.min(u32::MAX as u64) as u32;
                }
            }
            save_config_yaml(&config_dir.join("config.yaml"), &disk)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            write_active_profile_name(&profiles_dir, &resolved)?;
            println!(
                "Activated profile '{}' (requested '{}').",
                resolved, requested
            );
        }
        "delete" => {
            let requested = name.ok_or_else(|| {
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile delete <name>".into(),
                )
            })?;
            let resolved = resolve_profile_name(&requested, &aliases);
            let path = resolve_profile_yaml_path(&profiles_dir, &resolved).ok_or_else(|| {
                AgentError::Config(format!(
                    "Profile '{}' not found (resolved to '{}')",
                    requested, resolved
                ))
            })?;
            if !yes
                && !prompt_yes_no(
                    &format!("Delete profile '{}' ({})?", resolved, path.display()),
                    false,
                )
                .await?
            {
                println!("Aborted.");
                return Ok(());
            }
            std::fs::remove_file(&path)
                .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
            aliases.retain(|alias, target| alias != &requested && target != &resolved);
            save_profile_aliases(&aliases_path, &aliases)?;
            if read_active_profile_name(&profiles_dir).as_deref() == Some(resolved.as_str()) {
                let _ = std::fs::remove_file(active_profile_marker_path(&profiles_dir));
            }
            println!("Deleted profile '{}' ({})", resolved, path.display());
        }
        "alias" => {
            if remove {
                let alias = alias_name
                    .or(name)
                    .or(secondary)
                    .ok_or_else(|| AgentError::Config("profile alias --remove <alias>".into()))?;
                if aliases.remove(alias.trim()).is_some() {
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Removed alias '{}'.", alias.trim());
                } else {
                    println!("Alias '{}' not found.", alias.trim());
                }
                return Ok(());
            }
            let target = name.ok_or_else(|| {
                AgentError::Config(
                    "profile alias usage: hermes profile alias <target> --name <alias>".into(),
                )
            })?;
            let alias = alias_name.or(secondary).ok_or_else(|| {
                AgentError::Config(
                    "profile alias usage: hermes profile alias <target> --name <alias>".into(),
                )
            })?;
            let resolved_target = resolve_profile_name(&target, &aliases);
            if resolve_profile_yaml_path(&profiles_dir, &resolved_target).is_none() {
                return Err(AgentError::Config(format!(
                    "Alias target profile '{}' not found",
                    resolved_target
                )));
            }
            aliases.insert(alias.trim().to_string(), resolved_target.clone());
            save_profile_aliases(&aliases_path, &aliases)?;
            println!("Alias '{}' -> '{}'", alias.trim(), resolved_target);
        }
        "rename" => {
            let old_requested = name.ok_or_else(|| {
                AgentError::Config("profile rename usage: hermes profile rename <old> <new>".into())
            })?;
            let new_name = secondary.ok_or_else(|| {
                AgentError::Config("profile rename usage: hermes profile rename <old> <new>".into())
            })?;
            let new_name = validate_profile_name(&new_name)?;
            let old_resolved = resolve_profile_name(&old_requested, &aliases);
            let old_path =
                resolve_profile_yaml_path(&profiles_dir, &old_resolved).ok_or_else(|| {
                    AgentError::Config(format!("Profile '{}' not found", old_resolved))
                })?;
            let new_path = profiles_dir.join(format!("{}.yaml", new_name));
            if new_path.exists() {
                return Err(AgentError::Config(format!(
                    "Target profile '{}' already exists",
                    new_name
                )));
            }
            std::fs::rename(&old_path, &new_path).map_err(|e| {
                AgentError::Io(format!(
                    "rename {} -> {}: {}",
                    old_path.display(),
                    new_path.display(),
                    e
                ))
            })?;
            if let Ok(mut value) = load_profile_yaml(&new_path) {
                if let Some(map) = value.as_mapping_mut() {
                    map.insert(
                        serde_yaml::Value::String("name".to_string()),
                        serde_yaml::Value::String(new_name.clone()),
                    );
                    let _ = save_profile_yaml(&new_path, &value);
                }
            }
            for target in aliases.values_mut() {
                if target == &old_resolved {
                    *target = new_name.clone();
                }
            }
            if let Some(v) = aliases.remove(&old_requested) {
                aliases.insert(
                    new_name.clone(),
                    if v == old_resolved {
                        new_name.clone()
                    } else {
                        v
                    },
                );
            }
            save_profile_aliases(&aliases_path, &aliases)?;
            if read_active_profile_name(&profiles_dir).as_deref() == Some(old_resolved.as_str()) {
                write_active_profile_name(&profiles_dir, &new_name)?;
            }
            println!("Renamed profile '{}' -> '{}'", old_resolved, new_name);
        }
        "export" => {
            let target = if let Some(n) = name {
                resolve_profile_name(&n, &aliases)
            } else {
                read_active_profile_name(&profiles_dir).ok_or_else(|| {
                    AgentError::Config(
                        "profile export: no active profile and no name provided".into(),
                    )
                })?
            };
            let source = resolve_profile_yaml_path(&profiles_dir, &target)
                .ok_or_else(|| AgentError::Config(format!("Profile '{}' not found", target)))?;
            let out = output.unwrap_or_else(|| format!("{}.profile.yaml", target));
            std::fs::copy(&source, &out).map_err(|e| {
                AgentError::Io(format!("copy {} -> {}: {}", source.display(), out, e))
            })?;
            println!("Exported profile '{}' to {}", target, out);
        }
        "import" => {
            let source = name.ok_or_else(|| {
                AgentError::Config("profile import usage: hermes profile import <path>".into())
            })?;
            let source_path = PathBuf::from(&source);
            if !source_path.exists() {
                return Err(AgentError::Config(format!(
                    "profile import source not found: {}",
                    source_path.display()
                )));
            }
            let mut value = load_profile_yaml(&source_path)?;
            let target_name_raw = import_name.unwrap_or_else(|| {
                source_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            let target_name = validate_profile_name(&target_name_raw)?;
            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("mkdir {}: {}", profiles_dir.display(), e)))?;
            let target_path = profiles_dir.join(format!("{}.yaml", target_name));
            if target_path.exists() {
                let metadata = std::fs::metadata(&target_path).map_err(|e| {
                    AgentError::Io(format!("stat {}: {}", target_path.display(), e))
                })?;
                if metadata.is_dir() {
                    return Err(AgentError::Config(format!(
                        "Refusing to import profile: target path is a directory ({})",
                        target_path.display()
                    )));
                }
                if !yes {
                    return Err(AgentError::Config(format!(
                        "Target profile exists at {} (re-run with -y to overwrite)",
                        target_path.display()
                    )));
                }
            }
            if let Some(map) = value.as_mapping_mut() {
                map.insert(
                    serde_yaml::Value::String("name".to_string()),
                    serde_yaml::Value::String(target_name.clone()),
                );
            }
            let staged_path = profiles_dir.join(format!(
                ".{}.import-{}.yaml.tmp",
                target_name,
                uuid::Uuid::new_v4()
            ));
            save_profile_yaml(&staged_path, &value)?;
            if target_path.exists() {
                std::fs::remove_file(&target_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", target_path.display(), e))
                })?;
            }
            if let Err(err) = std::fs::rename(&staged_path, &target_path) {
                let _ = std::fs::remove_file(&staged_path);
                return Err(AgentError::Io(format!(
                    "rename {} -> {}: {}",
                    staged_path.display(),
                    target_path.display(),
                    err
                )));
            }
            if !no_alias {
                if let Some(alias) = alias_name
                    .or(secondary)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    aliases.insert(alias.clone(), target_name.clone());
                    save_profile_aliases(&aliases_path, &aliases)?;
                    println!("Added alias '{}' -> '{}'.", alias, target_name);
                }
            }
            println!(
                "Imported profile '{}' from {}",
                target_name,
                source_path.display()
            );
        }
        other => {
            println!(
                "Unknown profile action: '{}'. Use list|show|create|use|delete|alias|rename|export|import.",
                other
            );
        }
    }
    Ok(())
}

