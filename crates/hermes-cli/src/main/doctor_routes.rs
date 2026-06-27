/// Handle `hermes doctor`.
fn build_elite_doctor_diagnostics(cli: &Cli) -> serde_json::Value {
    let provenance_path = provenance_key_path_for_cli(cli);
    let provenance_exists = provenance_path.exists();
    let provenance_key_id = if provenance_exists {
        load_or_create_provenance_key(cli, false).ok().map(|key| {
            let digest = Sha256::digest(&key);
            let full = hex::encode(digest);
            full.chars().take(16).collect::<String>()
        })
    } else {
        None
    };

    let route_path = route_learning_state_path_for_cli(cli);
    let route_state = load_route_learning_state_for_cli(&route_path).ok();
    let route_entries = route_state
        .as_ref()
        .map(|state| state.entries.len())
        .unwrap_or(0usize);
    let route_health_path = route_health_state_path_for_cli(cli);
    let route_health_summary = std::fs::read_to_string(&route_health_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("summary").cloned());

    let policy_counters_path = default_tool_policy_counters_path();
    let policy_counters = load_tool_policy_counters(&policy_counters_path).unwrap_or_default();
    let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "enforce".to_string());
    let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "relaxed".to_string());

    let elite_gate_script = std::env::var("HERMES_ELITE_GATE_CMD")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "python3 scripts/run-elite-sync-gate.py".to_string());
    let gate_available = {
        let script_path = std::env::current_dir()
            .ok()
            .map(|cwd| cwd.join("scripts").join("run-elite-sync-gate.py"));
        script_path.as_ref().map(|p| p.exists()).unwrap_or(false)
    };

    serde_json::json!({
        "provenance": {
            "path": provenance_path.display().to_string(),
            "exists": provenance_exists,
            "key_id": provenance_key_id,
        },
        "route_learning": {
            "path": route_path.display().to_string(),
            "entries": route_entries,
            "ttl_secs": route_learning_ttl_secs(),
            "half_life_secs": route_learning_half_life_secs(),
            "saved_at_unix_ms": route_state.as_ref().map(|s| s.saved_at_unix_ms),
        },
        "route_health": {
            "path": route_health_path.display().to_string(),
            "available": route_health_summary.is_some(),
            "summary": route_health_summary,
        },
        "tool_policy": {
            "mode": policy_mode,
            "preset": policy_preset,
            "counters_path": policy_counters_path.display().to_string(),
            "counters": policy_counters,
        },
        "elite_gate": {
            "command": elite_gate_script,
            "script_available": gate_available,
        }
    })
}

async fn run_doctor(
    cli: Cli,
    deep: bool,
    self_heal: bool,
    snapshot: bool,
    snapshot_path: Option<String>,
    bundle: bool,
) -> Result<(), AgentError> {
    println!("Hermes Agent Ultra — System Check");
    println!("===========================\n");

    let mut checks: Vec<serde_json::Value> = Vec::new();
    let config_dir = hermes_config::hermes_home();
    let self_heal_actions = if self_heal {
        println!("Self-heal actions:");
        let actions = run_doctor_self_heal(&cli);
        for action in &actions {
            let status = action
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let detail = action.get("detail").and_then(|v| v.as_str()).unwrap_or("");
            println!("  - {}: {}", status, detail);
        }
        println!();
        checks.push(serde_json::json!({
            "name": "self_heal",
            "ok": actions.iter().all(|a| a.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)),
            "actions": actions,
        }));
        actions
    } else {
        Vec::new()
    };

    let config_dir_ok = config_dir.exists();
    print!("Config directory ({})... ", config_dir.display());
    if config_dir_ok {
        println!("✓");
    } else {
        println!("✗ (run `hermes-ultra setup`)");
    }
    checks.push(serde_json::json!({
        "name": "config_dir",
        "ok": config_dir_ok,
        "path": config_dir.display().to_string()
    }));

    let config_path = config_dir.join("config.yaml");
    let config_yaml_ok = config_path.exists();
    print!("config.yaml... ");
    if config_yaml_ok {
        println!("✓");
    } else {
        println!("✗ (run `hermes-ultra setup`)");
    }
    checks.push(serde_json::json!({
        "name": "config_yaml",
        "ok": config_yaml_ok,
        "path": config_path.display().to_string()
    }));

    let env_path = config_dir.join(".env");
    let project_env = std::env::current_dir()
        .ok()
        .map(|p| p.join(".env"))
        .filter(|p| p.exists());
    let env_ok = env_path.exists() || project_env.is_some();
    print!("~/.hermes/.env... ");
    if env_path.exists() {
        println!("✓");
    } else if let Some(ref p) = project_env {
        println!("✓ (using fallback {})", p.display());
    } else {
        println!("✗ (run `hermes-ultra setup`)");
    }
    checks.push(serde_json::json!({
        "name": "env_file",
        "ok": env_ok,
        "path": env_path.display().to_string(),
        "fallback": project_env.as_ref().map(|p| p.display().to_string()),
    }));

    let soul_path = config_dir.join("SOUL.md");
    let soul_ok = soul_path.exists();
    print!("SOUL.md persona file... ");
    if soul_ok {
        println!("✓");
    } else {
        println!("✗ (will be created by `hermes-ultra setup` or installer)");
    }
    checks.push(serde_json::json!({
        "name": "soul_md",
        "ok": soul_ok,
        "path": soul_path.display().to_string()
    }));

    let env_file = config_dir.join(".env");
    let project_env_file = std::env::current_dir().ok().map(|p| p.join(".env"));
    let has_key = |key: &str| -> bool {
        std::env::var(key)
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
            || read_env_key(&env_file, key)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            || project_env_file
                .as_ref()
                .and_then(|p| read_env_key(p, key))
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
    };

    let api_checks = [
        ("HERMES_OPENAI_API_KEY", "OpenAI (Hermes)"),
        ("OPENAI_API_KEY", "OpenAI"),
        ("ANTHROPIC_API_KEY", "Anthropic"),
        ("OPENROUTER_API_KEY", "OpenRouter"),
        ("NOUS_API_KEY", "Nous"),
        ("EXA_API_KEY", "Exa (web search)"),
        ("FIRECRAWL_API_KEY", "Firecrawl (web extract)"),
    ];

    println!("\nAPI Keys:");
    for (env_var, name) in &api_checks {
        let ok = has_key(env_var);
        print!("  {} ({})... ", name, env_var);
        if ok {
            println!("✓");
        } else {
            println!("✗ (not set)");
        }
        checks.push(serde_json::json!({
            "name": format!("api_key_{env_var}"),
            "ok": ok
        }));
    }

    println!("\nExternal tools:");
    let tool_checks = [
        ("docker", "Docker", false),
        ("ssh", "SSH", false),
        ("git", "Git", false),
        ("node", "Node.js", true),
        ("agent-browser", "agent-browser", true),
    ];

    for (cmd, name, optional) in &tool_checks {
        print!("  {}... ", name);
        let ok = match tokio::process::Command::new("which")
            .arg(cmd)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                println!("✓");
                true
            }
            _ if *optional => {
                println!("(optional, not found)");
                true
            }
            _ => {
                println!("✗ (not found)");
                false
            }
        };
        checks.push(serde_json::json!({
            "name": format!("bin_{cmd}"),
            "ok": ok,
            "optional": optional
        }));
    }

    let mut config_summary = serde_json::json!({
        "loaded": false
    });
    let mut loaded_config: Option<GatewayConfig> = None;
    println!("\nConfiguration:");
    print!("  Loading config... ");
    match load_config(cli.config_dir.as_deref()) {
        Ok(config) => {
            println!("✓");
            println!(
                "  Model: {}",
                config.model.as_deref().unwrap_or("(default)")
            );
            println!("  Max turns: {}", config.max_turns);
            let platform_count = config.platforms.iter().filter(|(_, p)| p.enabled).count();
            println!("  Enabled platforms: {}", platform_count);
            loaded_config = Some(config.clone());
            config_summary = serde_json::json!({
                "loaded": true,
                "model": config.model,
                "max_turns": config.max_turns,
                "enabled_platforms": platform_count,
            });
            checks.push(serde_json::json!({
                "name": "config_load",
                "ok": true
            }));
        }
        Err(e) => {
            println!("✗ ({})", e);
            checks.push(serde_json::json!({
                "name": "config_load",
                "ok": false,
                "error": e.to_string()
            }));
        }
    }

    println!("\nLocal backend endpoints:");
    for spec in hermes_provider_runtime::local_backend_specs() {
        let provider = spec.provider;
        let base_url = hermes_provider_runtime::local_backend_resolved_base_url(
            provider,
            loaded_config.as_ref(),
        );
        let (reachable, probed_url) = if let Some(url) = base_url.clone() {
            let models_url = format!("{}/models", url.trim_end_matches('/'));
            let ok = reqwest::Client::new()
                .get(models_url.as_str())
                .timeout(std::time::Duration::from_millis(900))
                .send()
                .await
                .map(|resp| resp.status().is_success())
                .unwrap_or(false);
            (ok, Some(models_url))
        } else {
            (false, None)
        };

        println!(
            "  {:<24} ... {}",
            spec.display_name,
            if reachable {
                "✓ reachable"
            } else {
                "(optional, endpoint not reachable)"
            }
        );
        checks.push(serde_json::json!({
            "name": format!("local_backend_{provider}"),
            "ok": true,
            "provider": provider,
            "display_name": spec.display_name,
            "base_url_env_var": spec.base_url_env_var,
            "base_url": base_url,
            "probe_url": probed_url,
            "reachable": reachable,
            "optional": true
        }));
    }

    if deep {
        println!("\nDeep diagnostics:");
        let svc = gateway_service_status()?;
        let svc_ok = svc.is_some();
        println!(
            "  gateway service... {}",
            if svc_ok { "✓" } else { "(not detected)" }
        );
        checks.push(serde_json::json!({
            "name": "gateway_service_status",
            "ok": true,
            "detail": svc
        }));

        let pid_path = gateway_pid_path_for_cli(&cli);
        let pid = read_gateway_pid(&pid_path);
        let pid_alive = pid.map(gateway_pid_is_alive).unwrap_or(false);
        println!(
            "  gateway pid... {}",
            if pid_alive { "✓" } else { "(not running)" }
        );
        checks.push(serde_json::json!({
            "name": "gateway_pid",
            "ok": pid_alive,
            "pid": pid,
            "pid_path": pid_path.display().to_string()
        }));

        let cl_health = reqwest::Client::new()
            .get("http://127.0.0.1:8075/health")
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map(|resp| resp.status().is_success())
            .unwrap_or(false);
        println!(
            "  contextlattice health... {}",
            if cl_health { "✓" } else { "✗" }
        );
        checks.push(serde_json::json!({
            "name": "contextlattice_health",
            "ok": cl_health,
            "url": "http://127.0.0.1:8075/health"
        }));

        let replay_dir = hermes_state_root(&cli).join("logs").join("replay");
        let replay_summaries = replay_integrity_summaries(&replay_dir, 5);
        let replay_count = std::fs::read_dir(&replay_dir)
            .map(|it| {
                it.filter_map(|e| e.ok().filter(|e| e.path().is_file()).map(|_| ()))
                    .count()
            })
            .unwrap_or(0usize);
        let replay_chain_ok = replay_summaries
            .iter()
            .all(|entry| entry.hash_chain_ok && entry.invalid_lines == 0);
        println!(
            "  replay traces... {} ({} files, chain {})",
            if replay_count > 0 { "✓" } else { "(none)" },
            replay_count,
            if replay_chain_ok { "ok" } else { "warn" }
        );
        checks.push(serde_json::json!({
            "name": "replay_traces",
            "ok": true,
            "dir": replay_dir.display().to_string(),
            "count": replay_count,
            "chain_ok": replay_chain_ok,
            "summaries": replay_summaries
        }));
    }

    let elite = build_elite_doctor_diagnostics(&cli);
    println!("\nElite diagnostics:");
    println!(
        "  provenance key... {}",
        if elite["provenance"]["exists"].as_bool().unwrap_or(false) {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "  route-learning entries... {}",
        elite["route_learning"]["entries"].as_u64().unwrap_or(0)
    );
    println!(
        "  route-health... {}",
        if elite["route_health"]["available"]
            .as_bool()
            .unwrap_or(false)
        {
            elite["route_health"]["summary"]["overall"]
                .as_str()
                .unwrap_or("available")
        } else {
            "(not generated)"
        }
    );
    println!(
        "  tool-policy mode/preset... {}/{}",
        elite["tool_policy"]["mode"].as_str().unwrap_or("unknown"),
        elite["tool_policy"]["preset"].as_str().unwrap_or("unknown")
    );
    println!(
        "  elite gate script... {}",
        if elite["elite_gate"]["script_available"]
            .as_bool()
            .unwrap_or(false)
        {
            "✓"
        } else {
            "✗"
        }
    );
    checks.push(serde_json::json!({
        "name": "elite_diagnostics",
        "ok": true,
        "details": elite,
    }));

    let snapshot_payload = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "deep": deep,
        "self_heal": self_heal,
        "self_heal_actions": self_heal_actions,
        "state_root": hermes_state_root(&cli).display().to_string(),
        "checks": checks,
        "config_summary": config_summary,
        "elite": build_elite_doctor_diagnostics(&cli),
    });

    let mut snapshot_written: Option<PathBuf> = None;
    if snapshot || bundle {
        let out = write_doctor_snapshot(&cli, &snapshot_payload, snapshot_path.as_deref())?;
        println!("\nDoctor snapshot: {}", out.display());
        if let Ok(snapshot_bytes) = std::fs::read(&out) {
            match sign_artifact_bytes(&cli, &snapshot_bytes, true)
                .and_then(|sig| write_provenance_sidecar(&out, &sig))
            {
                Ok(sig_path) => {
                    println!("Snapshot signature: {}", sig_path.display());
                    checks.push(serde_json::json!({
                        "name": "snapshot_provenance",
                        "ok": true,
                        "signature_path": sig_path.display().to_string(),
                    }));
                }
                Err(err) => {
                    checks.push(serde_json::json!({
                        "name": "snapshot_provenance",
                        "ok": false,
                        "error": err.to_string(),
                    }));
                }
            }
        }
        snapshot_written = Some(out);
    }

    if bundle {
        let snapshot_path = snapshot_written.as_ref().ok_or_else(|| {
            AgentError::Config("doctor bundle requires snapshot path".to_string())
        })?;
        let bundle_path = build_doctor_support_bundle(&cli, snapshot_path)?;
        println!("Support bundle: {}", bundle_path.display());
    }

    println!("\nDone.");
    Ok(())
}

fn run_doctor_self_heal(cli: &Cli) -> Vec<serde_json::Value> {
    let mut actions = Vec::new();
    let state_root = hermes_state_root(cli);
    let required_dirs = [
        state_root.clone(),
        state_root.join("profiles"),
        state_root.join("sessions"),
        state_root.join("logs"),
        state_root.join("skills"),
        state_root.join("auth"),
        state_root.join("snapshots"),
    ];

    for dir in required_dirs {
        if dir.exists() {
            actions.push(serde_json::json!({
                "ok": true,
                "status": "exists",
                "detail": format!("directory {}", dir.display()),
            }));
            continue;
        }
        match std::fs::create_dir_all(&dir) {
            Ok(_) => actions.push(serde_json::json!({
                "ok": true,
                "status": "created",
                "detail": format!("directory {}", dir.display()),
            })),
            Err(err) => actions.push(serde_json::json!({
                "ok": false,
                "status": "error",
                "detail": format!("directory {}: {}", dir.display(), err),
            })),
        }
    }

    let persistence = SessionPersistence::new(&state_root);
    if let Some(reason) = persistence.db_health_error() {
        if SessionPersistence::is_malformed_db_error_message(&reason) {
            let report = persistence.repair_malformed_schema(true);
            actions.push(serde_json::json!({
                "ok": report.repaired,
                "status": if report.repaired { "fixed" } else { "error" },
                "detail": if report.repaired {
                    format!(
                        "repaired malformed sessions.db schema using {} (backup: {})",
                        report.strategy.as_deref().unwrap_or("unknown"),
                        report
                            .backup_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "not created".to_string())
                    )
                } else {
                    format!(
                        "sessions.db schema repair failed: {}",
                        report
                            .error
                            .as_deref()
                            .unwrap_or("repair did not return a concrete error")
                    )
                },
            }));
        } else {
            actions.push(serde_json::json!({
                "ok": false,
                "status": "warn",
                "detail": format!("sessions.db does not open cleanly: {reason}"),
            }));
        }
    }

    let pid_path = gateway_pid_path_for_cli(cli);
    if pid_path.exists() {
        match read_gateway_pid(&pid_path) {
            Some(pid) if !gateway_pid_is_alive(pid) => match std::fs::remove_file(&pid_path) {
                Ok(_) => actions.push(serde_json::json!({
                    "ok": true,
                    "status": "fixed",
                    "detail": format!("removed stale gateway pid file {} (pid {})", pid_path.display(), pid),
                })),
                Err(err) => actions.push(serde_json::json!({
                    "ok": false,
                    "status": "error",
                    "detail": format!("remove stale pid {} failed: {}", pid_path.display(), err),
                })),
            },
            Some(pid) => actions.push(serde_json::json!({
                "ok": true,
                "status": "noop",
                "detail": format!("gateway pid {} is active", pid),
            })),
            None => actions.push(serde_json::json!({
                "ok": true,
                "status": "noop",
                "detail": format!("pid file {} is unreadable; left unchanged", pid_path.display()),
            })),
        }
    }

    let vault_path = secret_vault_path_for_cli(cli);
    if vault_path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::metadata(&vault_path) {
                Ok(meta) => {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode != 0o600 {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o600);
                        match std::fs::set_permissions(&vault_path, perms) {
                            Ok(_) => actions.push(serde_json::json!({
                                "ok": true,
                                "status": "fixed",
                                "detail": format!("normalized permissions on {} to 600", vault_path.display()),
                            })),
                            Err(err) => actions.push(serde_json::json!({
                                "ok": false,
                                "status": "error",
                                "detail": format!("set permissions on {} failed: {}", vault_path.display(), err),
                            })),
                        }
                    } else {
                        actions.push(serde_json::json!({
                            "ok": true,
                            "status": "noop",
                            "detail": format!("permissions already secure on {}", vault_path.display()),
                        }));
                    }
                }
                Err(err) => actions.push(serde_json::json!({
                    "ok": false,
                    "status": "error",
                    "detail": format!("metadata {} failed: {}", vault_path.display(), err),
                })),
            }
        }
        #[cfg(not(unix))]
        {
            actions.push(serde_json::json!({
                "ok": true,
                "status": "noop",
                "detail": format!("permission normalization skipped on non-unix for {}", vault_path.display()),
            }));
        }
    }

    actions
}

fn write_doctor_snapshot(
    cli: &Cli,
    snapshot_payload: &serde_json::Value,
    requested_path: Option<&str>,
) -> Result<PathBuf, AgentError> {
    let path = if let Some(raw) = requested_path.map(str::trim).filter(|s| !s.is_empty()) {
        PathBuf::from(raw)
    } else {
        hermes_state_root(cli).join("snapshots").join(format!(
            "doctor-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let body = serde_json::to_string_pretty(snapshot_payload)
        .map_err(|e| AgentError::Config(format!("serialize doctor snapshot: {}", e)))?;
    std::fs::write(&path, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
    Ok(path)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProvenanceSignature {
    generated_at: String,
    algorithm: String,
    key_id: String,
    artifact_sha256: String,
    signature_hex: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ProvenanceVerification {
    ok: bool,
    code: String,
    key_id: Option<String>,
    artifact_sha256: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct RouteLearningStatsRecord {
    samples: u32,
    success_rate: f64,
    avg_latency_ms: f64,
    consecutive_failures: u32,
    updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct RouteLearningStateRecord {
    schema_version: u32,
    saved_at_unix_ms: i64,
    entries: std::collections::HashMap<String, RouteLearningStatsRecord>,
}

fn provenance_key_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("auth").join("provenance.key")
}

fn parse_provenance_key_material(raw: &str) -> Result<Vec<u8>, AgentError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(AgentError::Config(
            "empty provenance key material".to_string(),
        ));
    }
    let is_hex = s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex {
        return hex::decode(s)
            .map_err(|e| AgentError::Config(format!("decode provenance hex key: {e}")));
    }
    if let Ok(bytes) = BASE64_STANDARD.decode(s.as_bytes()) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    Ok(s.as_bytes().to_vec())
}

fn load_or_create_provenance_key(cli: &Cli, allow_create: bool) -> Result<Vec<u8>, AgentError> {
    if let Ok(raw_env) = std::env::var("HERMES_PROVENANCE_SIGNING_KEY") {
        let bytes = parse_provenance_key_material(&raw_env)?;
        if bytes.len() < 16 {
            return Err(AgentError::Config(
                "HERMES_PROVENANCE_SIGNING_KEY must be at least 16 bytes".to_string(),
            ));
        }
        return Ok(bytes);
    }

    let path = provenance_key_path_for_cli(cli);
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
        let bytes = parse_provenance_key_material(&raw)?;
        if bytes.len() < 16 {
            return Err(AgentError::Config(format!(
                "provenance key in {} must be at least 16 bytes",
                path.display()
            )));
        }
        return Ok(bytes);
    }

    if !allow_create {
        return Err(AgentError::Config(format!(
            "provenance key not found at {} (set HERMES_PROVENANCE_SIGNING_KEY or run doctor snapshot/bundle once)",
            path.display()
        )));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let mut key_bytes = [0u8; 32];
    rand::fill(&mut key_bytes[..]);
    let key_hex = hex::encode(key_bytes);
    std::fs::write(&path, format!("{key_hex}\n"))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .map_err(|e| AgentError::Io(format!("metadata {}: {}", path.display(), e)))?
            .permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }
    Ok(key_bytes.to_vec())
}

fn sign_artifact_bytes(
    cli: &Cli,
    bytes: &[u8],
    allow_create_key: bool,
) -> Result<ProvenanceSignature, AgentError> {
    use hmac::Mac as _;

    let key = load_or_create_provenance_key(cli, allow_create_key)?;
    let artifact_hash_bytes = Sha256::digest(bytes);
    let artifact_sha256 = hex::encode(artifact_hash_bytes);
    let key_id = {
        let key_hash = Sha256::digest(&key);
        let full = hex::encode(key_hash);
        full.chars().take(16).collect::<String>()
    };
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
        .map_err(|e| AgentError::Config(format!("init provenance hmac: {e}")))?;
    mac.update(artifact_sha256.as_bytes());
    let signature_hex = hex::encode(mac.finalize().into_bytes());
    Ok(ProvenanceSignature {
        generated_at: chrono::Utc::now().to_rfc3339(),
        algorithm: "hmac-sha256".to_string(),
        key_id,
        artifact_sha256,
        signature_hex,
    })
}

fn provenance_sidecar_path_for_artifact(path: &Path) -> PathBuf {
    let filename = path
        .file_name()
        .map(|f| format!("{}.sig.json", f.to_string_lossy()))
        .unwrap_or_else(|| "artifact.sig.json".to_string());
    path.parent()
        .map(|p| p.join(&filename))
        .unwrap_or_else(|| PathBuf::from(filename))
}

fn write_provenance_sidecar(path: &Path, sig: &ProvenanceSignature) -> Result<PathBuf, AgentError> {
    let sidecar = provenance_sidecar_path_for_artifact(path);
    let body = serde_json::to_string_pretty(sig)
        .map_err(|e| AgentError::Config(format!("serialize provenance sidecar: {e}")))?;
    std::fs::write(&sidecar, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", sidecar.display(), e)))?;
    Ok(sidecar)
}

fn verify_artifact_provenance(
    cli: &Cli,
    artifact_path: &Path,
    signature_path: Option<&Path>,
) -> Result<ProvenanceVerification, AgentError> {
    use hmac::Mac as _;

    let bytes = match std::fs::read(artifact_path) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "artifact_read_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("read {}: {}", artifact_path.display(), err)),
            });
        }
    };
    let sidecar_path = signature_path
        .map(PathBuf::from)
        .unwrap_or_else(|| provenance_sidecar_path_for_artifact(artifact_path));
    let sidecar_raw = match std::fs::read_to_string(&sidecar_path) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "signature_read_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("read {}: {}", sidecar_path.display(), err)),
            });
        }
    };
    let sig: ProvenanceSignature = match serde_json::from_str(&sidecar_raw) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "signature_parse_error".to_string(),
                key_id: None,
                artifact_sha256: None,
                reason: Some(format!("parse {}: {}", sidecar_path.display(), err)),
            });
        }
    };
    let key = match load_or_create_provenance_key(cli, false) {
        Ok(value) => value,
        Err(err) => {
            return Ok(ProvenanceVerification {
                ok: false,
                code: "key_unavailable".to_string(),
                key_id: Some(sig.key_id),
                artifact_sha256: Some(sig.artifact_sha256),
                reason: Some(err.to_string()),
            });
        }
    };
    let artifact_sha = hex::encode(Sha256::digest(&bytes));
    if artifact_sha != sig.artifact_sha256 {
        return Ok(ProvenanceVerification {
            ok: false,
            code: "artifact_sha256_mismatch".to_string(),
            key_id: Some(sig.key_id),
            artifact_sha256: Some(artifact_sha),
            reason: Some("artifact_sha256 mismatch".to_string()),
        });
    }
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
        .map_err(|e| AgentError::Config(format!("init provenance hmac: {e}")))?;
    mac.update(sig.artifact_sha256.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    if expected != sig.signature_hex {
        return Ok(ProvenanceVerification {
            ok: false,
            code: "signature_mismatch".to_string(),
            key_id: Some(sig.key_id),
            artifact_sha256: Some(sig.artifact_sha256),
            reason: Some("signature mismatch".to_string()),
        });
    }
    Ok(ProvenanceVerification {
        ok: true,
        code: "ok".to_string(),
        key_id: Some(sig.key_id),
        artifact_sha256: Some(sig.artifact_sha256),
        reason: None,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
struct ReplayIntegritySummary {
    file: String,
    checksum_sha256: Option<String>,
    events: usize,
    invalid_lines: usize,
    hash_chain_ok: bool,
    last_event_hash: Option<String>,
}

fn sha256_file_hex(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

fn replay_integrity_for_file(path: &Path) -> ReplayIntegritySummary {
    let mut events = 0usize;
    let mut invalid_lines = 0usize;
    let mut hash_chain_ok = true;
    let mut last_event_hash: Option<String> = None;
    let mut last_seq: Option<u64> = None;

    if let Ok(body) = std::fs::read_to_string(path) {
        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => {
                    invalid_lines = invalid_lines.saturating_add(1);
                    hash_chain_ok = false;
                    continue;
                }
            };
            events = events.saturating_add(1);
            let seq = parsed.get("seq").and_then(|v| v.as_u64());
            if let (Some(prev), Some(cur_seq)) = (last_seq, seq) {
                if cur_seq != prev.saturating_add(1) {
                    hash_chain_ok = false;
                }
            }
            if let Some(cur_seq) = seq {
                last_seq = Some(cur_seq);
            }
            let prev_hash = parsed
                .get("prev_hash")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let event_hash = parsed
                .get("event_hash")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if let (Some(expected_prev), Some(actual_prev)) =
                (last_event_hash.as_ref(), prev_hash.as_ref())
            {
                if expected_prev != actual_prev {
                    hash_chain_ok = false;
                }
            }
            if event_hash.is_none() {
                hash_chain_ok = false;
            }
            last_event_hash = event_hash.or(last_event_hash);
        }
    } else {
        invalid_lines = 1;
        hash_chain_ok = false;
    }

    ReplayIntegritySummary {
        file: path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string()),
        checksum_sha256: sha256_file_hex(path),
        events,
        invalid_lines,
        hash_chain_ok,
        last_event_hash,
    }
}

fn replay_integrity_summaries(replay_dir: &Path, limit: usize) -> Vec<ReplayIntegritySummary> {
    if !replay_dir.exists() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(replay_dir)
        .map(|rd| {
            rd.filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.reverse();
    files
        .into_iter()
        .take(limit)
        .map(|path| replay_integrity_for_file(&path))
        .collect()
}

fn replay_manifest_json(summaries: &[ReplayIntegritySummary]) -> serde_json::Value {
    let generated_at = if std::env::var("HERMES_DETERMINISTIC_ARTIFACTS")
        .ok()
        .map(|v| {
            let n = v.trim().to_ascii_lowercase();
            n == "1" || n == "true" || n == "yes" || n == "on"
        })
        .unwrap_or(true)
    {
        "1970-01-01T00:00:00Z".to_string()
    } else {
        chrono::Utc::now().to_rfc3339()
    };
    serde_json::json!({
        "generated_at": generated_at,
        "files": summaries,
        "totals": {
            "files": summaries.len(),
            "events": summaries.iter().map(|s| s.events).sum::<usize>(),
            "invalid_lines": summaries.iter().map(|s| s.invalid_lines).sum::<usize>(),
            "hash_chain_ok": summaries.iter().all(|s| s.hash_chain_ok && s.invalid_lines == 0),
        }
    })
}

fn append_bundle_bytes(
    tar: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>,
    name: &str,
    bytes: &[u8],
    deterministic: bool,
) -> Result<(), AgentError> {
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    if deterministic {
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
    }
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .map_err(|e| AgentError::Io(format!("append {}: {}", name, e)))
}

fn build_doctor_support_bundle_with_options(
    cli: &Cli,
    snapshot_path: &Path,
    output_path: Option<&Path>,
    deterministic: bool,
) -> Result<PathBuf, AgentError> {
    let reports_dir = debug_reports_dir_for_cli(cli);
    std::fs::create_dir_all(&reports_dir)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", reports_dir.display(), e)))?;
    let bundle_path = output_path.map(PathBuf::from).unwrap_or_else(|| {
        reports_dir.join(format!(
            "support-bundle-{}.tar.gz",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
    });
    if let Some(parent) = bundle_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let file = std::fs::File::create(&bundle_path)
        .map_err(|e| AgentError::Io(format!("create {}: {}", bundle_path.display(), e)))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);

    let snapshot_bytes = std::fs::read(snapshot_path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", snapshot_path.display(), e)))?;
    append_bundle_bytes(
        &mut tar,
        "doctor/snapshot.json",
        &snapshot_bytes,
        deterministic,
    )?;

    let report = collect_debug_report(cli, 200)?;
    append_bundle_bytes(
        &mut tar,
        "doctor/debug-report.md",
        report.as_bytes(),
        deterministic,
    )?;

    let state_root = hermes_state_root(cli);
    let log_files = [
        (
            "logs/hermes.log",
            state_root.join("logs").join("hermes.log"),
        ),
        (
            "logs/mcp-stderr.log",
            state_root.join("logs").join("mcp-stderr.log"),
        ),
    ];
    for (name, path) in log_files {
        if path.exists() {
            let bytes = std::fs::read(&path)
                .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
            append_bundle_bytes(&mut tar, &format!("doctor/{name}"), &bytes, deterministic)?;
        }
    }

    let replay_dir = state_root.join("logs").join("replay");
    let mut replay_manifest_entries: Vec<ReplayIntegritySummary> = Vec::new();
    if replay_dir.exists() {
        let mut replay_files: Vec<PathBuf> = std::fs::read_dir(&replay_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        replay_files.sort();
        replay_files.reverse();
        for path in replay_files.into_iter().take(5) {
            if path.is_file() {
                replay_manifest_entries.push(replay_integrity_for_file(&path));
                let name = format!(
                    "doctor/replay/{}",
                    path.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "replay.jsonl".to_string())
                );
                let bytes = std::fs::read(&path)
                    .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
                append_bundle_bytes(&mut tar, &name, &bytes, deterministic)?;
            }
        }
    }

    let manifest = replay_manifest_json(&replay_manifest_entries);
    let manifest_body = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| AgentError::Config(format!("serialize replay manifest: {}", e)))?;
    append_bundle_bytes(
        &mut tar,
        "doctor/replay/manifest.json",
        manifest_body.as_slice(),
        deterministic,
    )?;

    if let Ok(sig) = sign_artifact_bytes(cli, &manifest_body, true) {
        let sig_body = serde_json::to_vec_pretty(&sig)
            .map_err(|e| AgentError::Config(format!("serialize replay signature: {}", e)))?;
        append_bundle_bytes(
            &mut tar,
            "doctor/replay/manifest.sig.json",
            sig_body.as_slice(),
            deterministic,
        )?;
    }

    tar.finish()
        .map_err(|e| AgentError::Io(format!("finalize {}: {}", bundle_path.display(), e)))?;
    Ok(bundle_path)
}

fn build_doctor_support_bundle(cli: &Cli, snapshot_path: &Path) -> Result<PathBuf, AgentError> {
    build_doctor_support_bundle_with_options(cli, snapshot_path, None, false)
}

/// Handle `hermes update`.
async fn run_update(_check: bool) -> Result<(), AgentError> {
    println!("Hermes Agent v{}", env!("CARGO_PKG_VERSION"));
    println!("{}", hermes_cli::update::check_for_updates().await?);
    Ok(())
}

async fn run_elite_check(_cli: Cli, json: bool, strict: bool) -> Result<(), AgentError> {
    let base_cmd = std::env::var("HERMES_ELITE_GATE_CMD")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "python3 scripts/run-elite-sync-gate.py --repo-root .".to_string());
    let mut cmdline = base_cmd;
    if json {
        cmdline.push_str(" --json");
    }
    let output = tokio::process::Command::new("bash")
        .args(["-lc", &cmdline])
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("elite-check command failed to start: {}", e)))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        println!("{}", stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        eprintln!("{}", stderr.trim_end());
    }
    if strict && !output.status.success() {
        return Err(AgentError::Config(format!(
            "elite-check failed (status={})",
            output.status
        )));
    }
    Ok(())
}

async fn run_verify_provenance(
    cli: Cli,
    path: String,
    signature: Option<String>,
    strict: bool,
    json: bool,
) -> Result<(), AgentError> {
    let artifact = PathBuf::from(path);
    let signature_path = signature
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| Some(provenance_sidecar_path_for_artifact(&artifact)));
    let verification = verify_artifact_provenance(&cli, &artifact, signature_path.as_deref())?;
    let rendered = if json {
        serde_json::to_string(&verification)
            .map_err(|e| AgentError::Config(format!("serialize verification: {}", e)))?
    } else {
        serde_json::to_string_pretty(&verification)
            .map_err(|e| AgentError::Config(format!("serialize verification: {}", e)))?
    };
    if verification.ok {
        if !json {
            println!("Provenance verification: ✓");
        }
        println!("{rendered}");
        return Ok(());
    }
    if !json {
        println!("Provenance verification: ✗");
    }
    println!("{rendered}");
    if strict {
        return Err(AgentError::Config(
            verification.reason.clone().unwrap_or_else(|| {
                format!("provenance verification failed ({})", verification.code)
            }),
        ));
    }
    Ok(())
}

fn route_learning_state_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-learning.json")
}

fn route_learning_ttl_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(7 * 24 * 60 * 60)
}

fn route_learning_half_life_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(24 * 60 * 60)
}

fn route_learning_effective_stats(
    stats: &RouteLearningStatsRecord,
    now_ms: i64,
) -> Option<RouteLearningStatsRecord> {
    if stats.samples == 0 {
        return None;
    }
    let mut out = stats.clone();
    if out.updated_at_unix_ms <= 0 {
        return Some(out);
    }
    let age_ms = now_ms.saturating_sub(out.updated_at_unix_ms).max(0);
    let ttl_secs = route_learning_ttl_secs();
    if ttl_secs > 0 && age_ms >= ttl_secs.saturating_mul(1000) {
        return None;
    }
    let half_life_secs = route_learning_half_life_secs();
    if half_life_secs <= 0 || age_ms <= 0 {
        return Some(out);
    }
    let half_life_ms = (half_life_secs.saturating_mul(1000)) as f64;
    let decay = (0.5_f64)
        .powf((age_ms as f64) / half_life_ms)
        .clamp(0.0, 1.0);
    let baseline_success = 0.90;
    let baseline_latency = 1800.0;
    out.success_rate = baseline_success + (out.success_rate - baseline_success) * decay;
    out.avg_latency_ms = baseline_latency + (out.avg_latency_ms - baseline_latency) * decay;
    out.consecutive_failures = ((out.consecutive_failures as f64) * decay).round() as u32;
    out.samples = ((out.samples as f64) * decay).round().max(1.0) as u32;
    Some(out)
}

fn route_learning_score(stats: &RouteLearningStatsRecord) -> f64 {
    let success_rate = stats.success_rate;
    let latency_score = (1.0 / (1.0 + (stats.avg_latency_ms / 2500.0))).clamp(0.05, 1.0);
    let failure_penalty = (stats.consecutive_failures as f64 * 0.08).min(0.35);
    let exploration_bonus = {
        let coverage = (stats.samples.min(20) as f64) / 20.0;
        (1.0 - coverage) * 0.03
    };
    (success_rate * 0.60) + (latency_score * 0.30) + exploration_bonus - failure_penalty
}

fn load_route_learning_state_for_cli(path: &Path) -> Result<RouteLearningStateRecord, AgentError> {
    if !path.exists() {
        return Ok(RouteLearningStateRecord {
            schema_version: 1,
            saved_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            entries: std::collections::HashMap::new(),
        });
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

async fn run_route_learning(
    cli: Cli,
    action: Option<String>,
    json: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let path = route_learning_state_path_for_cli(&cli);
    match action.as_str() {
        "reset" | "clear" => {
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "path": path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-learning state cleared: {}", path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" => {}
        _ => {
            return Err(AgentError::Config(format!(
                "route-learning: unsupported action '{}'; use show/list/inspect/reset/clear",
                action
            )))
        }
    }

    let state = load_route_learning_state_for_cli(&path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut rows: Vec<(String, RouteLearningStatsRecord, f64)> = state
        .entries
        .iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(stats, now_ms).map(|effective| {
                (
                    key.clone(),
                    effective.clone(),
                    route_learning_score(&effective),
                )
            })
        })
        .collect();
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    if json {
        let body = serde_json::json!({
            "path": path.display().to_string(),
            "ttl_secs": route_learning_ttl_secs(),
            "half_life_secs": route_learning_half_life_secs(),
            "saved_at_unix_ms": state.saved_at_unix_ms,
            "entries": rows.iter().map(|(key, stats, score)| {
                serde_json::json!({
                    "key": key,
                    "score": score,
                    "stats": stats,
                })
            }).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&body)
                .map_err(|e| AgentError::Config(format!("serialize route-learning json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-learning state: {}", path.display());
    println!(
        "TTL={}s half_life={}s entries={}",
        route_learning_ttl_secs(),
        route_learning_half_life_secs(),
        rows.len()
    );
    if rows.is_empty() {
        println!("(no learned routes yet)");
        return Ok(());
    }
    println!();
    println!(
        "{:<42}  {:>7}  {:>8}  {:>10}  {:>8}  {:>14}",
        "ROUTE", "SCORE", "SUCCESS", "LAT_MS", "FAILURES", "UPDATED_AT_MS"
    );
    for (key, stats, score) in rows {
        println!(
            "{:<42}  {:>7.3}  {:>7.2}%  {:>10.1}  {:>8}  {:>14}",
            key,
            score,
            stats.success_rate * 100.0,
            stats.avg_latency_ms,
            stats.consecutive_failures,
            stats.updated_at_unix_ms
        );
    }
    Ok(())
}

fn route_health_state_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-health.json")
}

fn route_autotune_state_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli)
        .join("logs")
        .join("route-autotune.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RouteHealthEntry {
    key: String,
    health_score: f64,
    tier: String,
    reasons: Vec<String>,
    stats: RouteLearningStatsRecord,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RouteAutotunePlan {
    generated_at: String,
    learning_path: String,
    health_report_path: String,
    env_path: String,
    summary: serde_json::Value,
    confidence: String,
    reasons: Vec<String>,
    overrides: std::collections::BTreeMap<String, String>,
}

fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn clamp_i64(value: i64, min: i64, max: i64) -> i64 {
    value.max(min).min(max)
}

fn build_route_autotune_plan(
    cli: &Cli,
    learning_path: &Path,
    report_path: &Path,
    entries: &[RouteHealthEntry],
    summary: &serde_json::Value,
) -> RouteAutotunePlan {
    let total = entries.len() as f64;
    let healthy = summary.get("healthy").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let watch = summary.get("watch").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let degraded = summary
        .get("degraded")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as f64;
    let critical = summary
        .get("critical")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as f64;
    let avg_score = summary
        .get("average_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let unhealthy_ratio = if total > 0.0 {
        (degraded + critical) / total
    } else {
        0.0
    };
    let watch_ratio = if total > 0.0 { watch / total } else { 0.0 };

    let mut reasons = Vec::new();
    if total < 3.0 {
        reasons.push("low_evidence_sample".to_string());
    }
    if critical > 0.0 {
        reasons.push("critical_routes_detected".to_string());
    } else if degraded > 0.0 {
        reasons.push("degraded_routes_detected".to_string());
    } else if watch > 0.0 {
        reasons.push("watch_routes_detected".to_string());
    } else if healthy > 0.0 {
        reasons.push("routes_healthy".to_string());
    } else {
        reasons.push("no_routes_learned".to_string());
    }
    if avg_score < 0.45 {
        reasons.push("average_health_low".to_string());
    } else if avg_score >= 0.75 {
        reasons.push("average_health_high".to_string());
    }

    let confidence = if total >= 12.0 {
        "high"
    } else if total >= 5.0 {
        "medium"
    } else {
        "low"
    };

    let cheap_bias = if critical > 0.0 {
        0.16
    } else if unhealthy_ratio >= 0.50 {
        0.14
    } else if unhealthy_ratio >= 0.25 || watch_ratio > 0.45 {
        0.11
    } else if avg_score >= 0.78 {
        0.06
    } else {
        0.08
    };
    let switch_margin = if critical > 0.0 {
        0.07
    } else if degraded > 0.0 {
        0.05
    } else if watch > 0.0 {
        0.04
    } else {
        0.03
    };
    let alpha = if critical > 0.0 {
        0.35
    } else if degraded > 0.0 {
        0.28
    } else if watch > 0.0 {
        0.24
    } else {
        0.20
    };
    let ttl_secs = if critical > 0.0 {
        5 * 24 * 60 * 60
    } else if degraded > 0.0 {
        6 * 24 * 60 * 60
    } else {
        7 * 24 * 60 * 60
    };
    let half_life_secs = if critical > 0.0 {
        12 * 60 * 60
    } else if degraded > 0.0 {
        18 * 60 * 60
    } else if watch > 0.0 {
        22 * 60 * 60
    } else {
        24 * 60 * 60
    };

    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_ALPHA".to_string(),
        format!("{:.3}", clamp_f64(alpha, 0.01, 1.0)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS".to_string(),
        format!("{:.3}", clamp_f64(cheap_bias, -0.50, 0.50)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN".to_string(),
        format!("{:.3}", clamp_f64(switch_margin, 0.0, 0.50)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_TTL_SECS".to_string(),
        clamp_i64(ttl_secs, 0, 30 * 24 * 60 * 60).to_string(),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS".to_string(),
        clamp_i64(half_life_secs, 0, 30 * 24 * 60 * 60).to_string(),
    );

    RouteAutotunePlan {
        generated_at: chrono::Utc::now().to_rfc3339(),
        learning_path: learning_path.display().to_string(),
        health_report_path: report_path.display().to_string(),
        env_path: route_autotune_env_path_for_cli(cli).display().to_string(),
        summary: summary.clone(),
        confidence: confidence.to_string(),
        reasons,
        overrides,
    }
}

fn route_health_tier(stats: &RouteLearningStatsRecord, score: f64) -> (String, Vec<String>, f64) {
    let mut reasons = Vec::new();
    if stats.success_rate < 0.55 {
        reasons.push("low_success_rate".to_string());
    } else if stats.success_rate < 0.72 {
        reasons.push("recovering_success_rate".to_string());
    }
    if stats.consecutive_failures >= 5 {
        reasons.push("failure_streak_critical".to_string());
    } else if stats.consecutive_failures >= 3 {
        reasons.push("failure_streak_watch".to_string());
    }
    if stats.avg_latency_ms > 5000.0 {
        reasons.push("high_latency".to_string());
    } else if stats.avg_latency_ms > 3000.0 {
        reasons.push("latency_watch".to_string());
    }

    let health_score = ((score + 0.30) / 1.20).clamp(0.0, 1.0);
    let tier = if stats.consecutive_failures >= 5 || stats.success_rate < 0.55 {
        "critical"
    } else if health_score >= 0.72 {
        "healthy"
    } else if health_score >= 0.52 {
        "watch"
    } else if health_score >= 0.35 {
        "degraded"
    } else {
        "critical"
    };
    (tier.to_string(), reasons, health_score)
}

async fn run_route_health(cli: Cli, action: Option<String>, json: bool) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let report_path = route_health_state_path_for_cli(&cli);

    match action.as_str() {
        "reset" | "clear" => {
            if report_path.exists() {
                std::fs::remove_file(&report_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", report_path.display(), e))
                })?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "path": report_path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-health report cleared: {}", report_path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" => {}
        _ => {
            return Err(AgentError::Config(format!(
                "route-health: unsupported action '{}'; use show/list/inspect/reset/clear",
                action
            )))
        }
    }

    let learning_path = route_learning_state_path_for_cli(&cli);
    let state = load_route_learning_state_for_cli(&learning_path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut entries: Vec<RouteHealthEntry> = state
        .entries
        .into_iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(&stats, now_ms).map(|effective| {
                let score = route_learning_score(&effective);
                let (tier, reasons, health_score) = route_health_tier(&effective, score);
                RouteHealthEntry {
                    key,
                    health_score,
                    tier,
                    reasons,
                    stats: effective,
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });

    let healthy = entries.iter().filter(|e| e.tier == "healthy").count();
    let watch = entries.iter().filter(|e| e.tier == "watch").count();
    let degraded = entries.iter().filter(|e| e.tier == "degraded").count();
    let critical = entries.iter().filter(|e| e.tier == "critical").count();
    let overall = if critical > 0 {
        "critical"
    } else if degraded > 0 {
        "degraded"
    } else if watch > 0 {
        "watch"
    } else if healthy > 0 {
        "healthy"
    } else {
        "unknown"
    };
    let avg_score = if entries.is_empty() {
        0.0
    } else {
        entries.iter().map(|e| e.health_score).sum::<f64>() / (entries.len() as f64)
    };

    let payload = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "path": report_path.display().to_string(),
        "learning_path": learning_path.display().to_string(),
        "summary": {
            "entries": entries.len(),
            "overall": overall,
            "average_score": avg_score,
            "healthy": healthy,
            "watch": watch,
            "degraded": degraded,
            "critical": critical,
        },
        "entries": entries,
    });

    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| AgentError::Config(format!("serialize route-health: {}", e)))?;
    std::fs::write(&report_path, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", report_path.display(), e)))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize route-health json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-health report: {}", report_path.display());
    println!(
        "Overall={} entries={} avg_score={:.3} (healthy={} watch={} degraded={} critical={})",
        overall,
        payload["summary"]["entries"].as_u64().unwrap_or(0),
        avg_score,
        healthy,
        watch,
        degraded,
        critical
    );
    if let Some(items) = payload["entries"].as_array() {
        if items.is_empty() {
            println!("(no routes learned yet)");
            return Ok(());
        }
        println!(
            "{:<42}  {:>7}  {:<9}  {:>8}  {:>10}  {:>8}",
            "ROUTE", "HEALTH", "TIER", "SUCCESS", "LAT_MS", "FAILURES"
        );
        for item in items {
            let key = item["key"].as_str().unwrap_or("");
            let health = item["health_score"].as_f64().unwrap_or(0.0);
            let tier = item["tier"].as_str().unwrap_or("unknown");
            let stats = item["stats"].as_object();
            let success = stats
                .and_then(|s| s.get("success_rate"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let latency = stats
                .and_then(|s| s.get("avg_latency_ms"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let failures = stats
                .and_then(|s| s.get("consecutive_failures"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            println!(
                "{:<42}  {:>7.3}  {:<9}  {:>7.2}%  {:>10.1}  {:>8}",
                key,
                health,
                tier,
                success * 100.0,
                latency,
                failures
            );
        }
    }
    Ok(())
}

async fn run_route_autotune(
    cli: Cli,
    action: Option<String>,
    apply: bool,
    strict: bool,
    json: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let route_report_path = route_health_state_path_for_cli(&cli);
    let autotune_state_path = route_autotune_state_path_for_cli(&cli);
    let autotune_env_path = route_autotune_env_path_for_cli(&cli);

    match action.as_str() {
        "reset" | "clear" => {
            if autotune_state_path.exists() {
                std::fs::remove_file(&autotune_state_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", autotune_state_path.display(), e))
                })?;
            }
            if autotune_env_path.exists() {
                std::fs::remove_file(&autotune_env_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", autotune_env_path.display(), e))
                })?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "state_path": autotune_state_path.display().to_string(),
                "env_path": autotune_env_path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-autotune artifacts cleared.");
                println!("State: {}", autotune_state_path.display());
                println!("Env:   {}", autotune_env_path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" | "plan" | "apply" => {}
        _ => {
            return Err(AgentError::Config(format!(
            "route-autotune: unsupported action '{}'; use show/list/inspect/plan/apply/reset/clear",
            action
        )))
        }
    }

    let learning_path = route_learning_state_path_for_cli(&cli);
    let state = load_route_learning_state_for_cli(&learning_path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut entries: Vec<RouteHealthEntry> = state
        .entries
        .into_iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(&stats, now_ms).map(|effective| {
                let score = route_learning_score(&effective);
                let (tier, reasons, health_score) = route_health_tier(&effective, score);
                RouteHealthEntry {
                    key,
                    health_score,
                    tier,
                    reasons,
                    stats: effective,
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });

    let healthy = entries.iter().filter(|e| e.tier == "healthy").count();
    let watch = entries.iter().filter(|e| e.tier == "watch").count();
    let degraded = entries.iter().filter(|e| e.tier == "degraded").count();
    let critical = entries.iter().filter(|e| e.tier == "critical").count();
    let overall = if critical > 0 {
        "critical"
    } else if degraded > 0 {
        "degraded"
    } else if watch > 0 {
        "watch"
    } else if healthy > 0 {
        "healthy"
    } else {
        "unknown"
    };
    let avg_score = if entries.is_empty() {
        0.0
    } else {
        entries.iter().map(|e| e.health_score).sum::<f64>() / (entries.len() as f64)
    };

    let summary = serde_json::json!({
        "entries": entries.len(),
        "overall": overall,
        "average_score": avg_score,
        "healthy": healthy,
        "watch": watch,
        "degraded": degraded,
        "critical": critical,
    });
    let plan =
        build_route_autotune_plan(&cli, &learning_path, &route_report_path, &entries, &summary);
    if strict && plan.confidence == "low" {
        return Err(AgentError::Config(
            "route-autotune strict mode requires at least 5 learned routes".to_string(),
        ));
    }

    if let Some(parent) = autotune_state_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    std::fs::write(
        &autotune_state_path,
        serde_json::to_string_pretty(&plan)
            .map_err(|e| AgentError::Config(format!("serialize route-autotune plan: {}", e)))?,
    )
    .map_err(|e| AgentError::Io(format!("write {}: {}", autotune_state_path.display(), e)))?;

    let should_apply = apply || action == "apply";
    if should_apply {
        if let Some(parent) = autotune_env_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
        }
        let mut body = String::new();
        body.push_str("# Hermes Agent Ultra route-autotune overrides\n");
        body.push_str(&format!("# generated_at={}\n", plan.generated_at));
        for (key, value) in &plan.overrides {
            body.push_str(&format!("{key}={value}\n"));
        }
        std::fs::write(&autotune_env_path, body)
            .map_err(|e| AgentError::Io(format!("write {}: {}", autotune_env_path.display(), e)))?;
    }

    let payload = serde_json::json!({
        "ok": true,
        "action": action,
        "applied": should_apply,
        "strict": strict,
        "state_path": autotune_state_path.display().to_string(),
        "env_path": autotune_env_path.display().to_string(),
        "plan": plan,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize route-autotune json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-autotune plan: {}", autotune_state_path.display());
    println!(
        "Overall={} entries={} avg_score={:.3} confidence={} applied={}",
        payload["plan"]["summary"]["overall"]
            .as_str()
            .unwrap_or("unknown"),
        payload["plan"]["summary"]["entries"].as_u64().unwrap_or(0),
        payload["plan"]["summary"]["average_score"]
            .as_f64()
            .unwrap_or(0.0),
        payload["plan"]["confidence"].as_str().unwrap_or("low"),
        if should_apply { "yes" } else { "no" },
    );
    if let Some(reasons) = payload["plan"]["reasons"].as_array() {
        if !reasons.is_empty() {
            println!(
                "Reasons: {}",
                reasons
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    println!("\nSuggested overrides:");
    if let Some(obj) = payload["plan"]["overrides"].as_object() {
        for (key, value) in obj {
            println!("  {:<44} {}", key, value.as_str().unwrap_or(""));
        }
    }
    if should_apply {
        println!(
            "\nApplied overrides file: {} (loaded automatically on next start unless env explicitly overrides a key)",
            autotune_env_path.display()
        );
    } else {
        println!("\nRun `hermes route-autotune apply --apply` to persist these overrides.");
    }
    Ok(())
}

async fn run_incident_pack(
    cli: Cli,
    snapshot: Option<String>,
    output: Option<String>,
    json: bool,
) -> Result<(), AgentError> {
    let snapshot_path = if let Some(path) = snapshot
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        if !path.exists() {
            return Err(AgentError::Config(format!(
                "incident-pack snapshot not found: {}",
                path.display()
            )));
        }
        path
    } else {
        let payload = serde_json::json!({
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "mode": "incident_pack_snapshot",
            "state_root": hermes_state_root(&cli).display().to_string(),
            "elite": build_elite_doctor_diagnostics(&cli),
        });
        let out = write_doctor_snapshot(&cli, &payload, None)?;
        if let Ok(snapshot_bytes) = std::fs::read(&out) {
            if let Ok(sig) = sign_artifact_bytes(&cli, &snapshot_bytes, true) {
                let _ = write_provenance_sidecar(&out, &sig);
            }
        }
        out
    };

    let output_path = output
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let bundle = build_doctor_support_bundle_with_options(
        &cli,
        &snapshot_path,
        output_path.as_deref(),
        true,
    )?;

    let bundle_sig_path = if let Ok(bundle_bytes) = std::fs::read(&bundle) {
        sign_artifact_bytes(&cli, &bundle_bytes, true)
            .ok()
            .and_then(|sig| write_provenance_sidecar(&bundle, &sig).ok())
            .map(|p| p.display().to_string())
    } else {
        None
    };

    let payload = serde_json::json!({
        "ok": true,
        "deterministic": true,
        "snapshot": snapshot_path.display().to_string(),
        "bundle": bundle.display().to_string(),
        "bundle_signature": bundle_sig_path,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize incident-pack json: {}", e)))?
        );
    } else {
        println!("Incident pack created: {}", bundle.display());
        println!("Snapshot: {}", snapshot_path.display());
        if let Some(sig) = payload["bundle_signature"].as_str() {
            println!("Bundle signature: {}", sig);
        }
    }
    Ok(())
}

async fn run_rotate_provenance_key(cli: Cli, json: bool) -> Result<(), AgentError> {
    let path = provenance_key_path_for_cli(&cli);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }

    let archived_path = if path.exists() {
        let archived = path.with_file_name(format!(
            "provenance.key.{}.bak",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::rename(&path, &archived)
            .map_err(|e| AgentError::Io(format!("archive {}: {}", path.display(), e)))?;
        Some(archived)
    } else {
        None
    };

    let mut key_bytes = [0u8; 32];
    rand::fill(&mut key_bytes[..]);
    let key_hex = hex::encode(key_bytes);
    std::fs::write(&path, format!("{key_hex}\n"))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .map_err(|e| AgentError::Io(format!("metadata {}: {}", path.display(), e)))?
            .permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }

    let key_id = {
        let digest = Sha256::digest(key_bytes);
        let full = hex::encode(digest);
        full.chars().take(16).collect::<String>()
    };
    let payload = serde_json::json!({
        "ok": true,
        "key_path": path.display().to_string(),
        "key_id": key_id,
        "archived_previous_key": archived_path.as_ref().map(|p| p.display().to_string()),
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize rotate response: {}", e)))?
        );
    } else {
        println!("Rotated provenance signing key.");
        println!("Active key: {}", path.display());
        if let Some(prev) = archived_path {
            println!("Archived previous key: {}", prev.display());
        }
        println!("New key id: {}", key_id);
    }
    Ok(())
}

