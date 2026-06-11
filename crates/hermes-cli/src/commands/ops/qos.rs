use std::fmt::Write as _;

use hermes_core::AgentError;

use super::super::read_json_file;
use super::route::{
    route_autotune_env_path, route_autotune_state_path, route_health_state_path,
    route_learning_state_path, summarize_route_health_details, summarize_route_health_state,
};
use super::shell::run_current_hermes_cli_command;
use crate::commands::{CommandResult, emit_command_output};

pub(crate) async fn handle_qos_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match sub.as_str() {
        "status" | "show" => {
            let learning_path = route_learning_state_path();
            let health_path = route_health_state_path();
            let autotune_path = route_autotune_state_path();
            let autotune_env = route_autotune_env_path();
            let learning_entries = read_json_file(&learning_path)
                .and_then(|v| {
                    v.get("entries")
                        .and_then(|e| e.as_array())
                        .map(|arr| arr.len())
                })
                .unwrap_or(0usize);
            let health_summary = summarize_route_health_state(&health_path);
            let mut out = String::new();
            let _ = writeln!(out, "Provider QoS router");
            let _ = writeln!(
                out,
                "  route_learning_entries={} ({})",
                learning_entries,
                learning_path.display()
            );
            let _ = writeln!(out, "  {} ({})", health_summary, health_path.display());
            if let Some(trace) = summarize_route_health_details(&health_path) {
                let _ = writeln!(out, "  {}", trace);
            }
            let _ = writeln!(
                out,
                "  route_autotune_state={} ({})",
                if autotune_path.exists() {
                    "present"
                } else {
                    "missing"
                },
                autotune_path.display()
            );
            let _ = writeln!(
                out,
                "  route_autotune_env={} ({})",
                if autotune_env.exists() {
                    "present"
                } else {
                    "missing"
                },
                autotune_env.display()
            );
            let _ = writeln!(
                out,
                "  actions: /qos health | /qos autotune plan | /qos autotune apply"
            );
            emit_command_output(host, out.trim_end());
            Ok(CommandResult::Handled)
        }
        "health" => {
            let out = run_current_hermes_cli_command(&["route-health", "--json"]).await?;
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        "autotune" => {
            let action = args.get(1).copied().unwrap_or("plan").to_ascii_lowercase();
            let out = match action.as_str() {
                "plan" => {
                    run_current_hermes_cli_command(&["route-autotune", "plan", "--json"]).await?
                }
                "apply" => {
                    run_current_hermes_cli_command(&[
                        "route-autotune",
                        "apply",
                        "--apply",
                        "--json",
                    ])
                    .await?
                }
                _ => {
                    emit_command_output(host, "Usage: /qos autotune [plan|apply]");
                    return Ok(CommandResult::Handled);
                }
            };
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        "help" => {
            emit_command_output(host, "Usage: /qos [status|health|autotune [plan|apply]]");
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(host, "Usage: /qos [status|health|autotune [plan|apply]]");
            Ok(CommandResult::Handled)
        }
    }
}
