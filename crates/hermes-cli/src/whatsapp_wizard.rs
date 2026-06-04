//! WhatsApp Rust client CLI wizard.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use hermes_config::{GatewayConfig, PlatformConfig};
use hermes_gateway::platforms::whatsapp::{
    clear_pairing_session, has_legacy_baileys_session, is_paired, mark_paired, session_db_path,
    WhatsAppConfig, WhatsAppRustClient,
};

/// Result of the interactive WhatsApp Web (QR) setup flow.
#[derive(Debug, Clone)]
pub struct WhatsAppSetupResult {
    pub mode: String,
    pub allow_from: Vec<String>,
    pub paired: bool,
}

pub fn whatsapp_session_path() -> PathBuf {
    hermes_config::hermes_home().join("whatsapp").join("session")
}

/// Menu label for `hermes gateway setup` (distinct from "configured" = enabled + paired).
pub fn whatsapp_gateway_menu_status(platform: Option<&PlatformConfig>) -> &'static str {
    let paired = is_paired(&whatsapp_session_path());
    let enabled = platform.is_some_and(|p| p.enabled);
    if enabled && paired {
        "configured"
    } else if paired {
        "paired, not enabled"
    } else {
        "not configured"
    }
}

fn prompt_line(label: &str) -> Result<String, hermes_core::AgentError> {
    print!("{label}");
    io::stdout()
        .flush()
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let line = io::stdin()
        .lock()
        .lines()
        .next()
        .transpose()
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?
        .unwrap_or_default();
    Ok(line.trim().to_string())
}

async fn prompt_whatsapp_mode_and_allowlist(
) -> Result<(String, Vec<String>), hermes_core::AgentError> {
    println!("Choose mode:");
    println!("  1) self-chat — message yourself on WhatsApp (quick test / personal)");
    println!("  2) bot — dedicated bot number (recommended for multi-user)");
    let mode_choice = prompt_line("Mode [1/2] (default 1): ")?;
    let mode = if mode_choice == "2" {
        "bot".to_string()
    } else {
        "self-chat".to_string()
    };

    let mut allow_from = Vec::new();
    if mode == "bot" {
        let users = prompt_line(
            "Allowed users (comma-separated phone numbers, or * for open bot): ",
        )?;
        if !users.is_empty() {
            allow_from = users
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Ok((mode, allow_from))
}

async fn run_qr_pairing(session: &Path) -> Result<bool, hermes_core::AgentError> {
    println!("\nStarting QR pairing — scan with WhatsApp → Linked Devices.\n");
    if session.exists() && session.read_dir()?.next().is_some() {
        println!("Clearing stale session files for fresh QR pairing...");
        clear_pairing_session(session).map_err(|e| {
            hermes_core::AgentError::Io(format!("Failed to clear WhatsApp session: {e}"))
        })?;
    }
    println!("Session database: {}\n", session_db_path(session).display());

    let mut cfg = WhatsAppConfig::default();
    cfg.session_path = Some(session.to_string_lossy().into_owned());
    let client = WhatsAppRustClient::new(cfg);

    match client.run_pairing().await {
        Ok(()) if is_paired(session) => Ok(true),
        Ok(()) => {
            println!("\nPairing did not complete.");
            Ok(false)
        }
        Err(e) => {
            println!("\nPairing failed: {e}");
            Ok(false)
        }
    }
}

fn set_whatsapp_env(mode: &str, allow_from: &[String], enable: bool) {
    // SAFETY: setup wizards run single-threaded during CLI setup.
    unsafe {
        std::env::set_var("WHATSAPP_MODE", mode);
        if enable {
            std::env::set_var("WHATSAPP_ENABLED", "true");
        }
        if allow_from.is_empty() {
            std::env::remove_var("WHATSAPP_ALLOWED_USERS");
        } else {
            std::env::set_var("WHATSAPP_ALLOWED_USERS", allow_from.join(","));
        }
    }
}

/// Merge WhatsApp Web settings into an in-memory gateway config (used by `hermes gateway setup`).
pub fn apply_whatsapp_to_gateway_config(
    disk: &mut GatewayConfig,
    mode: &str,
    allow_from: &[String],
) {
    let wa = disk
        .platforms
        .entry("whatsapp".to_string())
        .or_insert_with(PlatformConfig::default);
    wa.enabled = true;
    wa.extra
        .insert("mode".to_string(), serde_json::json!(mode));
    if !allow_from.is_empty() {
        wa.extra.insert(
            "allow_from".to_string(),
            serde_json::Value::Array(
                allow_from
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    set_whatsapp_env(mode, allow_from, true);
}

fn persist_whatsapp_config_yaml(
    mode: &str,
    allow_from: &[String],
    enable: bool,
) -> Result<(), hermes_core::AgentError> {
    let config_path = hermes_config::hermes_home().join("config.yaml");
    let mut config: serde_yaml::Value = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {e}")))?;
        serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Mapping(Default::default()))
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };

    let platforms = config
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("platforms".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa = platforms
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("whatsapp".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa_map = wa.as_mapping_mut().unwrap();
    wa_map.insert(
        serde_yaml::Value::String("enabled".into()),
        serde_yaml::Value::Bool(enable),
    );
    let mut extra = serde_yaml::Mapping::new();
    extra.insert(
        serde_yaml::Value::String("mode".into()),
        serde_yaml::Value::String(mode.to_string()),
    );
    if !allow_from.is_empty() {
        extra.insert(
            serde_yaml::Value::String("allow_from".into()),
            serde_yaml::Value::Sequence(
                allow_from
                    .iter()
                    .map(|u| serde_yaml::Value::String(u.clone()))
                    .collect(),
            ),
        );
    }
    if !extra.is_empty() {
        wa_map.insert(
            serde_yaml::Value::String("extra".into()),
            serde_yaml::Value::Mapping(extra),
        );
    }

    std::fs::create_dir_all(hermes_config::hermes_home())
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let yaml_str = serde_yaml::to_string(&config)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
    std::fs::write(&config_path, yaml_str)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    set_whatsapp_env(mode, allow_from, enable);
    Ok(())
}

async fn ensure_legacy_migration_ok(session: &Path) -> Result<bool, hermes_core::AgentError> {
    if has_legacy_baileys_session(session) && !is_paired(session) {
        println!(
            "\nLegacy Baileys session found at {}.",
            session.display()
        );
        println!("The Rust client uses a new SQLite session — you must re-pair.");
        let cont = prompt_line("Continue with new pairing? [Y/n]: ")?;
        if cont.eq_ignore_ascii_case("n") {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn prompt_existing_paired_session(
    session: &Path,
    mode: &str,
    allow_from: &[String],
    gateway: Option<&GatewayConfig>,
) -> Result<Option<WhatsAppSetupResult>, hermes_core::AgentError> {
    if !is_paired(session) {
        return Ok(None);
    }

    if let Some(disk) = gateway {
        let enabled = disk.platforms.get("whatsapp").is_some_and(|p| p.enabled);
        if enabled {
            println!("\nWhatsApp is already configured (paired session at {}).", session.display());
            let re_pair = prompt_line("Re-pair with a new QR code? [y/N]: ")?;
            if !re_pair.eq_ignore_ascii_case("y") {
                return Ok(Some(WhatsAppSetupResult {
                    mode: mode.to_string(),
                    allow_from: allow_from.to_vec(),
                    paired: true,
                }));
            }
            return Ok(None);
        }
        println!("\nA paired WhatsApp session exists at {}.", session.display());
        println!("Gateway config does not have WhatsApp enabled yet.");
        let enable = prompt_line("Use this session and enable WhatsApp? [Y/n]: ")?;
        if !enable.eq_ignore_ascii_case("n") {
            return Ok(Some(WhatsAppSetupResult {
                mode: mode.to_string(),
                allow_from: allow_from.to_vec(),
                paired: true,
            }));
        }
        return Ok(None);
    }

    println!("\nExisting Rust session found at {}.", session.display());
    let keep = prompt_line("Keep this session (skip QR pairing)? [Y/n]: ")?;
    if !keep.eq_ignore_ascii_case("n") {
        return Ok(Some(WhatsAppSetupResult {
            mode: mode.to_string(),
            allow_from: allow_from.to_vec(),
            paired: true,
        }));
    }
    Ok(None)
}

/// Interactive QR setup shared by `hermes whatsapp` and `hermes gateway setup`.
pub async fn run_whatsapp_setup_interactive(
    gateway: Option<&GatewayConfig>,
) -> Result<WhatsAppSetupResult, hermes_core::AgentError> {
    let (mode, allow_from) = prompt_whatsapp_mode_and_allowlist().await?;
    let session = whatsapp_session_path();

    if !ensure_legacy_migration_ok(&session).await? {
        return Ok(WhatsAppSetupResult {
            mode,
            allow_from,
            paired: false,
        });
    }

    if let Some(existing) =
        prompt_existing_paired_session(&session, &mode, &allow_from, gateway).await?
    {
        return Ok(existing);
    }

    let paired = run_qr_pairing(&session).await?;
    Ok(WhatsAppSetupResult {
        mode,
        allow_from,
        paired,
    })
}

/// Configure WhatsApp inside `hermes gateway setup` (personal / bot QR pairing).
pub async fn configure_whatsapp_for_gateway(
    disk: &mut GatewayConfig,
) -> Result<(), hermes_core::AgentError> {
    println!("WhatsApp (personal QR / wa-rs)");
    println!("Scan with WhatsApp → Linked Devices to link this machine.\n");

    let result = run_whatsapp_setup_interactive(Some(disk)).await?;
    if result.paired {
        apply_whatsapp_to_gateway_config(disk, &result.mode, &result.allow_from);
        println!("\nWhatsApp enabled for gateway.");
    } else {
        println!("\nWhatsApp not enabled — complete QR pairing and try again.");
    }
    Ok(())
}

/// Interactive wa-rs setup wizard (`hermes whatsapp`).
pub async fn whatsapp_baileys_wizard() -> Result<(), hermes_core::AgentError> {
    println!("WhatsApp Setup (Rust / wa-rs)");
    println!("==============================\n");

    let result = run_whatsapp_setup_interactive(None).await?;
    if result.paired {
        persist_whatsapp_config_yaml(&result.mode, &result.allow_from, true)?;
        println!("\nPairing successful! WhatsApp is enabled.");
        println!("Run `hermes gateway` to connect.");
    } else if !result.mode.is_empty() {
        println!("WHATSAPP_ENABLED was not set — re-run when pairing succeeds.");
    }
    Ok(())
}

/// Show WhatsApp Rust client status.
pub async fn whatsapp_baileys_status() -> Result<(), hermes_core::AgentError> {
    println!("WhatsApp Status (Rust / wa-rs)");
    println!("--------------------------------");
    let session = whatsapp_session_path();
    let paired = is_paired(&session);
    let legacy = has_legacy_baileys_session(&session);
    println!("  Session dir:    {}", session.display());
    println!(
        "  Rust paired:    {}",
        if paired { "yes" } else { "no" }
    );
    if legacy {
        println!("  Legacy Baileys: creds.json present (re-pair if not migrated)");
    }
    println!("  SQLite db:      {}", session_db_path(&session).display());

    let config_path = hermes_config::hermes_home().join("config.yaml");
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
        let config: serde_yaml::Value =
            serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Null);
        let enabled = config
            .get("platforms")
            .and_then(|p| p.get("whatsapp"))
            .and_then(|w| w.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        println!("  Enabled:        {enabled}");
    } else {
        println!("  Enabled:        false (no config.yaml)");
    }

    if !paired {
        println!("  Run `hermes whatsapp` to pair via QR.");
    }
    Ok(())
}

pub async fn whatsapp_cloud_setup() -> Result<(), hermes_core::AgentError> {
    crate::commands::whatsapp_cloud_setup_impl().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_gateway_menu_status_labels() {
        let dir = tempfile::TempDir::new().unwrap();
        // SAFETY: single-threaded test; isolates session from the developer machine.
        unsafe {
            std::env::set_var("HERMES_HOME", dir.path());
        }
        assert_eq!(whatsapp_gateway_menu_status(None), "not configured");
        let mut p = PlatformConfig::default();
        p.enabled = true;
        assert_eq!(whatsapp_gateway_menu_status(Some(&p)), "not configured");
        mark_paired(&whatsapp_session_path()).unwrap();
        assert_eq!(whatsapp_gateway_menu_status(None), "paired, not enabled");
        p.enabled = true;
        assert_eq!(whatsapp_gateway_menu_status(Some(&p)), "configured");
    }

    #[test]
    fn apply_whatsapp_sets_mode_extra() {
        let mut disk = GatewayConfig::default();
        apply_whatsapp_to_gateway_config(&mut disk, "self-chat", &[]);
        let wa = disk.platforms.get("whatsapp").unwrap();
        assert!(wa.enabled);
        assert_eq!(
            wa.extra.get("mode").and_then(|v| v.as_str()),
            Some("self-chat")
        );
    }
}
