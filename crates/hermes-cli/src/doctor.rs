//! Doctor diagnostics and support bundle.
//!
//! Provides `hermes doctor` system check, self-heal actions, debug report
//! collection, replay integrity verification, and support bundle generation.

use std::path::{Path, PathBuf};

use hermes_core::AgentError;
use sha2::{Digest, Sha256};

use crate::gateway_process::{
    gateway_pid_is_alive, gateway_pid_path_for_cli, gateway_service_status, read_gateway_pid,
};
use crate::provenance::{
    load_or_create_provenance_key, provenance_key_path_for_cli,
    provenance_sidecar_path_for_artifact, sign_artifact_bytes, verify_artifact_provenance,
    write_provenance_sidecar,
};
use crate::route_learning::{
    load_route_learning_state_for_cli, route_health_state_path_for_cli,
    route_learning_half_life_secs, route_learning_state_path_for_cli, route_learning_ttl_secs,
};

// ---------------------------------------------------------------------------
// Elite diagnostics
// ---------------------------------------------------------------------------

pub(crate) fn build_elite_doctor_diagnostics(cli: &crate::Cli) -> serde_json::Value {
    let state_root = crate::hermes_state_root(cli);
    let provenance_path = provenance_key_path_for_cli(&state_root);
    let provenance_exists = provenance_path.exists();
    let provenance_key_id = if provenance_exists {
        load_or_create_provenance_key(&state_root, false)
            .ok()
            .map(|key| {
                let digest = Sha256::digest(&key);
                let full = hex::encode(digest);
                full.chars().take(16).collect::<String>()
            })
    } else {
        None
    };

    let route_path = route_learning_state_path_for_cli(&state_root);
    let route_state = load_route_learning_state_for_cli(&route_path).ok();
    let route_entries = route_state
        .as_ref()
        .map(|state| state.entries.len())
        .unwrap_or(0usize);
    let route_health_path = route_health_state_path_for_cli(&state_root);
    let route_health_summary = std::fs::read_to_string(&route_health_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("summary").cloned());

    let policy_counters_path = hermes_tools::default_tool_policy_counters_path();
    let policy_counters =
        hermes_tools::load_tool_policy_counters(&policy_counters_path).unwrap_or_default();
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

// ---------------------------------------------------------------------------
// Doctor system check
// ---------------------------------------------------------------------------

pub(crate) async fn run_doctor(
    cli: crate::Cli,
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
        println!("✗ (run `hermes setup`)");
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
        println!("✗ (run `hermes setup`)");
    }
    checks.push(serde_json::json!({
        "name": "config_yaml",
        "ok": config_yaml_ok,
        "path": config_path.display().to_string()
    }));

    if let Ok(cfg) = hermes_config::load_config(None) {
        for issue in hermes_gateway::evaluate_gateway_requirements(
            &cfg,
            hermes_gateway::RequirementScope::Doctor,
        ) {
            let ok = issue.severity != hermes_gateway::RequirementSeverity::Fatal;
            let label = format!("Gateway / {}", issue.platform);
            print!("{}... ", label);
            if ok {
                println!("✓");
            } else {
                println!("✗ ({})", issue.message);
            }
            checks.push(serde_json::json!({
                "name": label,
                "ok": ok,
                "code": issue.code,
                "detail": issue.message,
            }));
        }
    }

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
        println!("✗ (run `hermes setup`)");
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
        println!("✗ (will be created by `hermes setup` or installer)");
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
            || crate::setup::read_env_key(&env_file, key)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            || project_env_file
                .as_ref()
                .and_then(|p| crate::setup::read_env_key(p, key))
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
    let mut loaded_config: Option<hermes_config::GatewayConfig> = None;
    println!("\nConfiguration:");
    print!("  Loading config... ");
    match hermes_config::load_config(cli.config_dir.as_deref()) {
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
    for provider in [
        "ollama-local",
        "llama-cpp",
        "vllm",
        "mlx",
        "apple-ane",
        "sglang",
        "tgi",
    ] {
        let configured = loaded_config
            .as_ref()
            .and_then(|cfg| cfg.llm_providers.get(provider))
            .and_then(|entry| entry.base_url.clone())
            .filter(|value| !value.trim().is_empty());
        let env_override = crate::setup::local_backend_base_url_env_var(provider)
            .and_then(|name| std::env::var(name).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let base_url = configured.or(env_override).or_else(|| {
            crate::setup::setup_provider_default_base_url(provider).map(ToString::to_string)
        });

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
            "  {:<12} ... {}",
            provider,
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

        let state_root = crate::hermes_state_root(&cli);
        let pid_path = gateway_pid_path_for_cli(&state_root);
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

        let replay_dir = crate::hermes_state_root(&cli).join("logs").join("replay");
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
        "state_root": crate::hermes_state_root(&cli).display().to_string(),
        "checks": checks,
        "config_summary": config_summary,
        "elite": build_elite_doctor_diagnostics(&cli),
    });

    let mut snapshot_written: Option<PathBuf> = None;
    if snapshot || bundle {
        let out = write_doctor_snapshot(&cli, &snapshot_payload, snapshot_path.as_deref())?;
        println!("\nDoctor snapshot: {}", out.display());
        if let Ok(snapshot_bytes) = std::fs::read(&out) {
            match sign_artifact_bytes(&crate::hermes_state_root(&cli), &snapshot_bytes, true)
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

// ---------------------------------------------------------------------------
// Self-heal
// ---------------------------------------------------------------------------

pub(crate) fn run_doctor_self_heal(cli: &crate::Cli) -> Vec<serde_json::Value> {
    let mut actions = Vec::new();
    let state_root = crate::hermes_state_root(cli);
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

    let pid_path = gateway_pid_path_for_cli(&crate::hermes_state_root(cli));
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

    let vault_path = crate::secret_vault_path_for_cli(&crate::hermes_state_root(cli));
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

// ---------------------------------------------------------------------------
// Snapshot writer
// ---------------------------------------------------------------------------

pub(crate) fn write_doctor_snapshot(
    cli: &crate::Cli,
    snapshot_payload: &serde_json::Value,
    requested_path: Option<&str>,
) -> Result<PathBuf, AgentError> {
    let path = if let Some(raw) = requested_path.map(str::trim).filter(|s| !s.is_empty()) {
        PathBuf::from(raw)
    } else {
        crate::hermes_state_root(cli)
            .join("snapshots")
            .join(format!(
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

// ---------------------------------------------------------------------------
// Replay integrity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ReplayIntegritySummary {
    pub(crate) file: String,
    pub(crate) checksum_sha256: Option<String>,
    pub(crate) events: usize,
    pub(crate) invalid_lines: usize,
    pub(crate) hash_chain_ok: bool,
    pub(crate) last_event_hash: Option<String>,
}

fn sha256_file_hex(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

pub(crate) fn replay_integrity_for_file(path: &Path) -> ReplayIntegritySummary {
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

#[allow(unused)]
pub(crate) fn replay_manifest_json(summaries: &[ReplayIntegritySummary]) -> serde_json::Value {
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

// ---------------------------------------------------------------------------
// Support bundle
// ---------------------------------------------------------------------------

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

pub(crate) fn build_doctor_support_bundle_with_options(
    cli: &crate::Cli,
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

    let state_root = crate::hermes_state_root(cli);
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

    if let Ok(sig) = sign_artifact_bytes(&crate::hermes_state_root(cli), &manifest_body, true) {
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

fn build_doctor_support_bundle(
    cli: &crate::Cli,
    snapshot_path: &Path,
) -> Result<PathBuf, AgentError> {
    build_doctor_support_bundle_with_options(cli, snapshot_path, None, false)
}

// ---------------------------------------------------------------------------
// Debug report
// ---------------------------------------------------------------------------

pub(crate) fn debug_reports_dir_for_cli(cli: &crate::Cli) -> PathBuf {
    hermes_cli::paths::CliStateRoot::from_config_dir(cli.config_dir.as_deref().map(Path::new))
        .debug_reports_dir()
}

#[allow(unused)]
pub(crate) fn prune_old_debug_reports(path: &Path, expire_days: u32) -> Result<usize, AgentError> {
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
        let entry = entry.map_err(|e| AgentError::Io(e.to_string()))?;
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            if modified < cutoff {
                let path = entry.path();
                if path.is_file() {
                    std::fs::remove_file(&path)
                        .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
                    removed = removed.saturating_add(1);
                }
            }
        }
    }
    Ok(removed)
}

const DEBUG_LOG_SNAPSHOT_MAX_BYTES: usize = 100_000;
#[allow(unused)]
const DEBUG_PENDING_PASTES_FILE: &str = "pending-pastes.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct DebugLogSnapshot {
    pub(crate) tail_text: String,
    pub(crate) full_text: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingPasteDelete {
    pub(crate) url: String,
    pub(crate) expires_at_unix: i64,
}

pub(crate) fn debug_pending_pastes_path(reports_dir: &Path) -> PathBuf {
    reports_dir.join(DEBUG_PENDING_PASTES_FILE)
}

pub(crate) fn best_effort_sweep_expired_pending_pastes(reports_dir: &Path, now_unix: i64) -> usize {
    sweep_expired_pending_pastes(reports_dir, now_unix).unwrap_or(0)
}

pub(crate) fn sweep_expired_pending_pastes(
    reports_dir: &Path,
    now_unix: i64,
) -> Result<usize, AgentError> {
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
    let total = entries.len();
    let remaining: Vec<PendingPasteDelete> = entries
        .into_iter()
        .filter(|e| e.expires_at_unix > now_unix)
        .collect();
    let removed = total - remaining.len();
    if remaining.is_empty() {
        let _ = std::fs::remove_file(&store);
    } else {
        let body = serde_json::to_string_pretty(&remaining)
            .map_err(|e| AgentError::Config(format!("serialize pending pastes: {}", e)))?;
        std::fs::write(&store, body)
            .map_err(|e| AgentError::Io(format!("write {}: {}", store.display(), e)))?;
    }
    Ok(removed)
}

#[allow(unused)]
pub(crate) fn record_pending_paste(
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
        let content = std::fs::read_to_string(&store)
            .map_err(|e| AgentError::Io(format!("read {}: {}", store.display(), e)))?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };
    entries.push(PendingPasteDelete {
        url: trimmed.to_string(),
        expires_at_unix: now_unix.saturating_add(expire_days as i64 * 86_400),
    });
    let body = serde_json::to_string_pretty(&entries)
        .map_err(|e| AgentError::Config(format!("serialize pending pastes: {}", e)))?;
    std::fs::write(&store, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", store.display(), e)))?;
    Ok(())
}

pub(crate) fn capture_debug_log_snapshot(
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
    let content = match std::fs::read_to_string(log_file) {
        Ok(c) => c,
        Err(_) => {
            return DebugLogSnapshot {
                tail_text: "(unreadable)".to_string(),
                full_text: None,
            };
        }
    };
    let truncated: String = content.chars().take(max_bytes).collect();
    let lines: Vec<&str> = truncated.lines().collect();
    let tail_len = tail_lines.min(lines.len());
    let tail: Vec<&str> = lines.iter().rev().take(tail_len).rev().copied().collect();
    DebugLogSnapshot {
        tail_text: tail.join("\n"),
        full_text: Some(content.chars().take(max_bytes).collect()),
    }
}

pub(crate) fn collect_debug_report(cli: &crate::Cli, lines: u32) -> Result<String, AgentError> {
    let now = chrono::Utc::now().to_rfc3339();
    let root = crate::hermes_state_root(cli);
    let cfg_path = root.join("config.yaml");
    let log_file = root.join("logs").join("hermes.log");
    let mut report = String::new();
    report.push_str("# Hermes Debug Report\n\n");
    report.push_str(&format!("- generated_at: {}\n", now));
    report.push_str(&format!("- version: {}\n", env!("CARGO_PKG_VERSION")));
    report.push_str(&format!("- os: {}\n", std::env::consts::OS));
    report.push_str(&format!("- arch: {}\n", std::env::consts::ARCH));
    report.push('\n');
    report.push_str("## Config\n\n");
    report.push_str(&format!("- config.yaml exists: {}\n", cfg_path.exists()));
    report.push('\n');
    report.push_str("## Log tail\n\n");
    report.push_str("```\n");
    match capture_debug_log_snapshot(&log_file, lines as usize, DEBUG_LOG_SNAPSHOT_MAX_BYTES) {
        DebugLogSnapshot {
            tail_text,
            full_text: _,
        } => {
            report.push_str(&tail_text);
        }
    }
    report.push_str("\n```\n");
    Ok(report)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    static ENV_LOCK: OnceLock<()> = OnceLock::new();

    #[test]
    fn capture_debug_log_snapshot_preserves_boundary_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("hermes.log");
        std::fs::write(&log_path, "line1\nline2\nline3\n").expect("write log");
        let snap = capture_debug_log_snapshot(&log_path, 2, 100_000);
        assert_eq!(snap.tail_text, "line2\nline3");
    }

    #[test]
    fn capture_debug_log_snapshot_caps_memory_with_long_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("hermes.log");
        let long = "A".repeat(5000);
        std::fs::write(&log_path, &long).expect("write log");
        let snap = capture_debug_log_snapshot(&log_path, 10, 100);
        assert!(snap.tail_text.len() <= 100, "tail capped");
    }

    #[test]
    fn capture_debug_log_snapshot_distinguishes_missing_and_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing.log");
        let snap = capture_debug_log_snapshot(&missing, 5, 100_000);
        assert!(snap.tail_text.contains("not found"));
        assert!(snap.full_text.is_none());

        let empty = tmp.path().join("empty.log");
        std::fs::write(&empty, "").expect("write empty");
        let snap = capture_debug_log_snapshot(&empty, 5, 100_000);
        assert!(!snap.tail_text.contains("not found"));
    }

    #[test]
    fn sweep_expired_pending_pastes_is_best_effort_and_keeps_fresh_entries() {
        let _lock = ENV_LOCK.get_or_init(|| ());
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = tmp.path().join("pending-pastes.json");
        let entries = vec![
            PendingPasteDelete {
                url: "https://expired.example.com".to_string(),
                expires_at_unix: 1000,
            },
            PendingPasteDelete {
                url: "https://fresh.example.com".to_string(),
                expires_at_unix: 9999999999,
            },
        ];
        std::fs::write(
            &store,
            serde_json::to_string_pretty(&entries).expect("serialize"),
        )
        .expect("write");
        let removed = sweep_expired_pending_pastes(tmp.path(), 999999999);
        assert_eq!(removed.unwrap(), 1);
        let remaining: Vec<PendingPasteDelete> =
            serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn best_effort_sweep_handles_invalid_store_without_failing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = tmp.path().join("pending-pastes.json");
        std::fs::write(&store, "{bad json").expect("write");
        let removed = best_effort_sweep_expired_pending_pastes(tmp.path(), 1000);
        assert_eq!(removed, 0);
    }

    #[test]
    fn doctor_self_heal_creates_missing_state_dirs() {
        use clap::Parser;
        let _lock = ENV_LOCK.get_or_init(|| ());
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = crate::Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            tmp.path().to_str().unwrap(),
        ]);
        let actions = run_doctor_self_heal(&cli);
        assert!(
            actions.iter().any(|a| a["status"] == "created"),
            "should create dirs"
        );
        assert!(tmp.path().join("profiles").exists());
        assert!(tmp.path().join("sessions").exists());
    }

    #[test]
    fn doctor_self_heal_removes_stale_gateway_pid_file() {
        use clap::Parser;
        let _lock = ENV_LOCK.get_or_init(|| ());
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = crate::Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            tmp.path().to_str().unwrap(),
        ]);
        let state_root = crate::hermes_state_root(&cli);
        let pid_path = gateway_pid_path_for_cli(&state_root);
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&pid_path, "999999\n").expect("write pid");
        let actions = run_doctor_self_heal(&cli);
        assert!(
            actions.iter().any(|a| a["status"] == "fixed"),
            "should fix stale pid: {:?}",
            actions
        );
        assert!(!pid_path.exists(), "stale pid file should be removed");
    }

    #[test]
    fn doctor_elite_diagnostics_payload_has_required_sections() {
        use clap::Parser;
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = crate::Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            tmp.path().to_str().unwrap(),
        ]);
        let payload = build_elite_doctor_diagnostics(&cli);
        assert!(payload.get("provenance").is_some());
        assert!(payload.get("route_learning").is_some());
    }

    #[test]
    fn replay_integrity_detects_chain_break() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let replay = tmp.path().join("session.jsonl");
        std::fs::write(
            &replay,
            r#"{"seq": 1, "prev_hash": null, "event_hash": "abc"}
{"seq": 3, "prev_hash": "abc", "event_hash": "def"}
"#,
        )
        .expect("write");
        let summary = replay_integrity_for_file(&replay);
        assert!(!summary.hash_chain_ok);
    }

    #[test]
    fn replay_manifest_aggregates_counts() {
        let items = vec![
            ReplayIntegritySummary {
                file: "a.jsonl".to_string(),
                checksum_sha256: None,
                events: 10,
                invalid_lines: 1,
                hash_chain_ok: true,
                last_event_hash: None,
            },
            ReplayIntegritySummary {
                file: "b.jsonl".to_string(),
                checksum_sha256: None,
                events: 5,
                invalid_lines: 0,
                hash_chain_ok: true,
                last_event_hash: None,
            },
        ];
        let manifest = replay_manifest_json(&items);
        assert_eq!(manifest["totals"]["files"].as_u64(), Some(2));
        assert_eq!(manifest["totals"]["events"].as_u64(), Some(15));
    }
}
