use std::path::PathBuf;

use clap_complete::{Shell as CompletionShell, generate};
use hermes_cli::auth::DEFAULT_NOUS_PORTAL_URL;
use hermes_cli::cli::Cli;
use hermes_core::AgentError;
use sha2::{Digest, Sha256};

use crate::auth_main::{mask_secret, run_auth};
use crate::provenance::{provenance_sidecar_path_for_artifact, verify_artifact_provenance};
use hermes_cli::state_paths::hermes_state_root;

pub(crate) async fn run_dump(
    cli: Cli,
    session: Option<String>,
    output: Option<String>,
) -> Result<(), AgentError> {
    let home = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let sessions_dir = home.join("sessions");
    let session = session.unwrap_or_else(|| "latest".to_string());
    let out = output.unwrap_or_else(|| format!("{}.dump.json", session));
    let payload = serde_json::json!({
        "session": session,
        "source_dir": sessions_dir,
        "note": "Session export scaffold"
    });
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    )
    .map_err(|e| AgentError::Io(format!("Failed to write dump: {}", e)))?;
    println!("Wrote dump to {}", out);
    Ok(())
}

pub(crate) fn run_completion(shell: Option<String>) -> Result<(), AgentError> {
    let mut cmd = hermes_cli::completion_command();
    let sh = match shell.as_deref().unwrap_or("zsh") {
        "bash" => CompletionShell::Bash,
        "fish" => CompletionShell::Fish,
        "powershell" => CompletionShell::PowerShell,
        "elvish" => CompletionShell::Elvish,
        _ => CompletionShell::Zsh,
    };
    generate(sh, &mut cmd, "hermes-agent-ultra", &mut std::io::stdout());
    Ok(())
}

pub(crate) async fn run_uninstall(yes: bool) -> Result<(), AgentError> {
    let home = hermes_config::hermes_home();
    if !yes {
        println!("Uninstall is destructive. Re-run with `hermes uninstall --yes`.");
        return Ok(());
    }
    if home.exists() {
        std::fs::remove_dir_all(&home)
            .map_err(|e| AgentError::Io(format!("Failed to remove {}: {}", home.display(), e)))?;
        println!("Removed {}", home.display());
    } else {
        println!("Nothing to uninstall.");
    }
    Ok(())
}

pub(crate) async fn run_lumio(
    action: Option<String>,
    model: Option<String>,
) -> Result<(), AgentError> {
    match action.as_deref() {
        None | Some("login") => {
            hermes_cli::lumio::setup(model.as_deref(), true).await?;
        }
        Some("logout") => {
            hermes_cli::lumio::clear_token();
            println!("✅ Lumio token removed.");
        }
        Some("status") => match hermes_cli::lumio::load_token() {
            Some(t) => {
                let user = if t.username.is_empty() {
                    "(unknown)"
                } else {
                    &t.username
                };
                println!("Lumio: logged in as {}", user);
                println!("  API: {}", t.base_url);
                println!("  Token: {}", mask_secret(&t.token));
            }
            None => {
                println!("Lumio: not logged in");
                println!("  Run `hermes lumio` to login.");
            }
        },
        Some(other) => {
            println!(
                "Unknown lumio action: '{}'. Use: login, logout, status.",
                other
            );
        }
    }
    Ok(())
}

pub(crate) fn read_setup_stdin_line(stdin: &std::io::Stdin) -> String {
    use std::io::BufRead;
    let mut line = String::new();
    let mut reader = stdin.lock();
    reader.read_line(&mut line).ok();
    line
}

pub(crate) fn run_kanban(args: Vec<String>) -> Result<(), AgentError> {
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    println!("{}", hermes_cli::commands::run_kanban_command(&arg_refs)?);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortalActionKind {
    Setup,
    Info,
}

pub(crate) fn portal_action_kind(action: Option<&str>) -> Result<PortalActionKind, AgentError> {
    match action.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("setup" | "login" | "auth") => Ok(PortalActionKind::Setup),
        Some("info" | "status" | "check") => Ok(PortalActionKind::Info),
        Some(other) => Err(AgentError::Config(format!(
            "Unknown portal action '{other}'. Use `hermes portal` for setup or `hermes portal info` for status."
        ))),
    }
}

pub(crate) async fn run_portal(cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    match portal_action_kind(action.as_deref())? {
        PortalActionKind::Setup => {
            println!("Nous Portal setup ({DEFAULT_NOUS_PORTAL_URL})");
            run_auth(
                cli,
                Some("setup".to_string()),
                Some("nous".to_string()),
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        PortalActionKind::Info => {
            println!("Nous Portal info ({DEFAULT_NOUS_PORTAL_URL})");
            run_auth(
                cli,
                Some("status".to_string()),
                Some("nous".to_string()),
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
    }
}

pub(crate) async fn run_update(
    check: bool,
    yes: bool,
    rollback: bool,
    force: bool,
    source: Option<String>,
    channel: Option<String>,
) -> Result<(), AgentError> {
    hermes_cli::update::replace::cleanup_old();

    if rollback {
        return hermes_cli::update::replace::rollback();
    }

    if check {
        println!("Hermes Agent v{}", env!("CARGO_PKG_VERSION"));
        println!("{}", hermes_cli::update::check_for_updates().await?);
        return Ok(());
    }

    hermes_cli::update::perform_update(hermes_cli::update::UpdateOptions {
        yes,
        force,
        source,
        channel,
    })
    .await
}

pub(crate) async fn run_elite_check(_cli: Cli, json: bool, strict: bool) -> Result<(), AgentError> {
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

pub(crate) async fn run_verify_provenance(
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
    let verification = verify_artifact_provenance(
        &hermes_state_root(&cli),
        &artifact,
        signature_path.as_deref(),
    )?;
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

pub(crate) async fn run_rotate_provenance_key(cli: Cli, json: bool) -> Result<(), AgentError> {
    let path =
        hermes_cli::paths::CliStateRoot::from_state_root(&hermes_state_root(&cli)).provenance_key();
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
    {
        use rand::TryRng;
        rand::rngs::SysRng
            .try_fill_bytes(&mut key_bytes)
            .map_err(|e| AgentError::Config(e.to_string()))?;
    }
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
