use hermes_core::AgentError;

use super::super::discover_repo_root_for_about;
use super::reports::{latest_json_report, summarize_gate_report};
use super::shell::run_ops_shell_command;
use crate::commands::{CommandResult, emit_command_output};

pub(crate) async fn handle_ops_eval_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            host,
            "Eval controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    match sub.as_str() {
        "status" => {
            let latest = latest_json_report(&report_dir, "session-eval-harness-")
                .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"));
            if let Some(path) = latest {
                let summary = summarize_gate_report(&path, "eval")
                    .unwrap_or_else(|| format!("latest eval report: {}", path.display()));
                emit_command_output(
                    host,
                    format!(
                        "{summary}\nRun `/ops eval run` to generate a fresh session-backed report."
                    ),
                );
            } else {
                emit_command_output(
                    host,
                    "No eval reports found yet. Run `/ops eval run` to generate one.",
                );
            }
            Ok(CommandResult::Handled)
        }
        "run" => {
            let out = run_ops_shell_command(
                "python3 scripts/run-session-eval-harness.py --repo-root . --json",
            )
            .await?;
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        "latest" => {
            let Some(path) = latest_json_report(&report_dir, "session-eval-harness-")
                .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"))
            else {
                emit_command_output(host, "No eval reports found.");
                return Ok(CommandResult::Handled);
            };
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
            emit_command_output(
                host,
                format!(
                    "Latest eval report: {}\n{}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string()),
                    raw
                ),
            );
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(host, "Usage: /ops eval [status|run|latest]");
            Ok(CommandResult::Handled)
        }
    }
}
