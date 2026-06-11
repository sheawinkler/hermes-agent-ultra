use hermes_core::AgentError;

use super::super::discover_repo_root_for_about;
use super::reports::{
    latest_json_report, self_evolution_recommendations, summarize_self_evolution_report,
};
use super::shell::{run_ops_shell_command, shell_escape};
use crate::commands::{CommandResult, emit_command_output};

pub(crate) async fn handle_ops_evolve_command(
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
            "Self-evolution controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    match sub.as_str() {
        "status" => {
            let summary = latest_json_report(&report_dir, "self-evolution-loop-")
                .and_then(|p| summarize_self_evolution_report(&p, "self_evolution"))
                .unwrap_or_else(|| "self_evolution=unknown (no reports yet)".to_string());
            emit_command_output(
                host,
                format!(
                    "{}\nRun `/ops evolve run` to execute the loop now.",
                    summary
                ),
            );
            Ok(CommandResult::Handled)
        }
        "run" => {
            let cmd = if let Some(obj) = host.session_objective() {
                format!(
                    "python3 scripts/run-self-evolution-loop.py --json --objective {}",
                    shell_escape(obj)
                )
            } else {
                "python3 scripts/run-self-evolution-loop.py --json".to_string()
            };
            let out = run_ops_shell_command(&cmd).await?;
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        "recommend" | "recs" => {
            let Some(path) = latest_json_report(&report_dir, "self-evolution-loop-") else {
                emit_command_output(
                    host,
                    "No self-evolution reports found. Run `/ops evolve run` first.",
                );
                return Ok(CommandResult::Handled);
            };
            let recs = self_evolution_recommendations(&path);
            if recs.is_empty() {
                emit_command_output(
                    host,
                    format!(
                        "No recommendations found in {}.",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string())
                    ),
                );
            } else {
                let file_label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                emit_command_output(
                    host,
                    format!(
                        "Self-evolution recommendations ({file_label}):\n{}",
                        recs.join("\n")
                    ),
                );
            }
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(host, "Usage: /ops evolve [status|run|recommend]");
            Ok(CommandResult::Handled)
        }
    }
}
