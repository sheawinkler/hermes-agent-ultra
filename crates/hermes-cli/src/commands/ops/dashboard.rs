use hermes_core::AgentError;

use super::shell::dashboard_status_line_from_payload;
use crate::commands::{CommandResult, emit_command_output};

pub(crate) async fn handle_dashboard_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let mut params = serde_json::json!({
        "action": action
    });
    if let Some(host) = args.get(1) {
        params["host"] = serde_json::Value::String((*host).to_string());
    }
    if let Some(port) = args.get(2).and_then(|raw| raw.parse::<u16>().ok()) {
        params["port"] = serde_json::json!(port);
    }
    if args
        .iter()
        .any(|arg| arg.eq_ignore_ascii_case("--insecure"))
    {
        params["insecure"] = serde_json::json!(true);
    }

    let raw = host
        .tool_registry()
        .dispatch_async("dashboard_control", params)
        .await;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({"result": raw}));

    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        emit_command_output(host, format!("Dashboard command failed: {err}"));
        return Ok(CommandResult::Handled);
    }

    let rendered = match action.as_str() {
        "enable" | "on" => format!(
            "Dashboard enabled at {}\nConfig: {}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        "disable" | "off" => format!(
            "Dashboard disabled (URL: {})\nConfig: {}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        "url" => format!(
            "{}\n{}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        _ => format!(
            "{}\nConfig: {}",
            dashboard_status_line_from_payload(&parsed),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
    };
    emit_command_output(host, rendered);
    Ok(CommandResult::Handled)
}
