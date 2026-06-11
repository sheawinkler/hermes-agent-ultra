//! Gateway platform and integration control-plane slash commands.

use std::fmt::Write as _;
use std::path::PathBuf;

use hermes_core::AgentError;

use crate::app::App;
use crate::commands::policy;
use crate::commands::{CommandResult, emit_command_output, truncate_chars};
use crate::model_switch::curated_provider_slugs;

pub(crate) fn handle_platforms_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.platforms.is_empty() {
        emit_command_output(
            app,
            "No explicit gateway platform adapters configured (running in local CLI mode).",
        );
        return Ok(CommandResult::Handled);
    }
    let mut entries: Vec<_> = app.config.platforms.keys().cloned().collect();
    entries.sort();
    let mut out = String::from("Configured gateway platforms:\n");
    for p in entries {
        let _ = writeln!(out, "  - {}", p);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn integrations_snapshot_path(session_id: &str) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    hermes_config::hermes_home().join("logs").join(format!(
        "integrations-snapshot-{}-{}.json",
        session_id, stamp
    ))
}

fn render_integrations_repair_steps(
    provider: &str,
    auth_ok: bool,
    oauth_gate: Option<(bool, String)>,
    memory_probe: &str,
) -> String {
    let mut out = String::new();
    out.push_str("Integrations repair plan\n");
    out.push_str("------------------------\n");
    let _ = writeln!(out, "provider: {}", provider);
    if !auth_ok {
        out.push_str("- auth: FAIL -> run `/auth status` then `/auth verify` (or `hermes-ultra auth add`).\n");
    } else {
        out.push_str("- auth: PASS\n");
    }
    if let Some((ok, detail)) = oauth_gate {
        if ok {
            let _ = writeln!(out, "- oauth runtime gate: PASS ({})", detail);
        } else {
            let _ = writeln!(
                out,
                "- oauth runtime gate: FAIL ({}) -> rebuild/install latest CLI binary.",
                detail
            );
        }
    }
    if memory_probe.to_ascii_lowercase().starts_with("warn") {
        let _ = writeln!(
            out,
            "- contextlattice probe: {} -> verify local orchestrator and env vars (CONTEXTLATTICE_ORCHESTRATOR_URL/MEMMCP_ORCHESTRATOR_URL).",
            memory_probe
        );
    } else {
        let _ = writeln!(out, "- contextlattice probe: {}", memory_probe);
    }
    out.push_str(
        "- tools: run `/tools` and `/integrations status` to verify adapter registry health.\n",
    );
    out.push_str(
        "- walkthrough: run `/walkthrough next` to continue operator recovery sequence.\n",
    );
    out
}

pub(crate) async fn handle_integrations_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let provider = app.current_runtime_provider();
    let provider_cap = crate::providers::provider_capability_for(&provider);
    let oauth_capable = provider_cap
        .as_ref()
        .map(|cap| cap.oauth_supported)
        .unwrap_or(false);
    let managed_tools = provider_cap
        .as_ref()
        .map(|cap| cap.managed_tools_supported)
        .unwrap_or(false);
    let credential_present = crate::app::provider_api_key_from_env(&provider).is_some();
    let oauth_state_present = crate::auth::read_provider_auth_state(&provider)
        .ok()
        .flatten()
        .is_some();
    let auth_ok = credential_present || (oauth_capable && oauth_state_present);
    let oauth_gate = policy::oauth_runtime_gate_for_provider(&provider);
    let oauth_manifest_source = policy::oauth_min_version_for_provider(&provider)
        .map(|(_, source)| source)
        .unwrap_or_else(|| "n/a".to_string());

    let memory_url = std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .ok()
        .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8075".to_string());
    let memory_probe = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => {
            let health_url = format!("{}/health", memory_url.trim_end_matches('/'));
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => format!("PASS ({})", health_url),
                Ok(resp) => format!("WARN ({} status={})", health_url, resp.status()),
                Err(err) => format!(
                    "WARN ({} error={})",
                    health_url,
                    truncate_chars(&err.to_string(), 96)
                ),
            }
        }
        Err(err) => format!(
            "WARN (client build failed: {})",
            truncate_chars(&err.to_string(), 96)
        ),
    };

    let tools_count = app.tool_registry.list_tools().len();
    let plugins_count = super::discover_plugin_surface(true).len();
    let mcp_count = app.config.mcp_servers.len();
    let platforms_count = app.config.platforms.len();

    if action == "repair" {
        emit_command_output(
            app,
            render_integrations_repair_steps(&provider, auth_ok, oauth_gate.clone(), &memory_probe),
        );
        return Ok(CommandResult::Handled);
    }

    if action == "snapshot" {
        let path = integrations_snapshot_path(&app.session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!("Failed to create {}: {}", parent.display(), e))
            })?;
        }
        let payload = serde_json::json!({
            "captured_at": chrono::Utc::now().to_rfc3339(),
            "session_id": app.session_id,
            "provider": provider,
            "model": app.current_model,
            "auth": {
                "oauth_capable": oauth_capable,
                "managed_tools_supported": managed_tools,
                "credential_present": credential_present,
                "oauth_state_present": oauth_state_present,
                "status": if auth_ok { "PASS" } else { "FAIL" },
                "oauth_runtime_gate": oauth_gate.as_ref().map(|(ok, detail)| serde_json::json!({"ok": ok, "detail": detail})),
            },
            "panels": {
                "providers_count": curated_provider_slugs().len(),
                "platform_adapters": platforms_count,
                "mcp_servers": mcp_count,
                "plugins": plugins_count,
                "toolsets": app.config.platform_toolsets.len(),
                "registered_tools": tools_count,
                "contextlattice_url": memory_url,
                "memory_probe": memory_probe,
            }
        });
        let json = serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Io(format!("Failed to encode snapshot payload: {}", e)))?;
        std::fs::write(&path, json)
            .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
        emit_command_output(
            app,
            format!(
                "Integration snapshot exported:\n{}\nUse `/integrations repair` for remediation guidance.",
                path.display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let mut out = String::new();
    out.push_str("Integration Control Plane\n");
    out.push_str("=========================\n");

    if action == "status" || action == "all" || action == "auth" {
        out.push_str("Auth panel\n----------\n");
        let _ = writeln!(out, "provider: {}", provider);
        let _ = writeln!(out, "model: {}", app.current_model);
        let _ = writeln!(out, "oauth_capable: {}", oauth_capable);
        let _ = writeln!(out, "managed_tools_supported: {}", managed_tools);
        let _ = writeln!(out, "credential_present: {}", credential_present);
        let _ = writeln!(out, "oauth_state_present: {}", oauth_state_present);
        let _ = writeln!(out, "status: {}", if auth_ok { "PASS" } else { "FAIL" });
        let _ = writeln!(out, "oauth_manifest: {}", oauth_manifest_source);
        if let Some((gate_ok, gate_detail)) = oauth_gate.clone() {
            let _ = writeln!(
                out,
                "oauth_runtime_gate: {} ({})",
                if gate_ok { "PASS" } else { "FAIL" },
                gate_detail
            );
            if !gate_ok {
                out.push_str("remediation: upgrade runtime and retry auth.\n");
            }
        }
        out.push('\n');
    }

    if action == "status" || action == "all" || action == "providers" {
        let providers = curated_provider_slugs();
        out.push_str("Providers panel\n---------------\n");
        let _ = writeln!(out, "configured_providers: {}", providers.join(", "));
        let _ = writeln!(out, "provider_count: {}", providers.len());
        out.push('\n');
    }

    if action == "status" || action == "all" || action == "gateway" {
        out.push_str("Gateway panel\n-------------\n");
        let _ = writeln!(out, "platform_adapters: {}", platforms_count);
        let _ = writeln!(out, "mcp_servers: {}", mcp_count);
        let _ = writeln!(out, "plugins: {}", plugins_count);
        let _ = writeln!(out, "toolsets: {}", app.config.platform_toolsets.len());
        out.push('\n');
    }

    if action == "status" || action == "all" || action == "memory" {
        out.push_str("Memory panel\n------------\n");
        let _ = writeln!(out, "contextlattice_url: {}", memory_url);
        let _ = writeln!(out, "probe: {}", memory_probe);
        let _ = writeln!(out, "registered_tools: {}", tools_count);
        out.push('\n');
    }

    if !matches!(
        action.as_str(),
        "status" | "all" | "auth" | "providers" | "gateway" | "memory" | "repair" | "snapshot"
    ) {
        emit_command_output(
            app,
            "Usage: /integrations [status|all|auth|providers|gateway|memory|repair|snapshot]",
        );
        return Ok(CommandResult::Handled);
    }

    out.push_str("Next actions:\n");
    out.push_str("- `/boot` for startup readiness\n");
    out.push_str("- `/auth verify` for runtime credential hydration\n");
    out.push_str("- `/walkthrough next` for guided operator setup\n");
    out.push_str(
        "- `/integrations repair` for remediation plan and `/integrations snapshot` for export\n",
    );
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}
