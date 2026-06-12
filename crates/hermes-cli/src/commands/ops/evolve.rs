use hermes_core::AgentError;

use super::super::discover_repo_root_for_about;
use super::reports::{
    latest_json_report, self_evolution_recommendations, summarize_self_evolution_report,
};
use super::shell::{run_ops_shell_command, shell_escape};
use crate::commands::{CommandResult, emit_command_output};

fn runtime_evolve_status() -> String {
    let home = hermes_config::paths::hermes_home();
    let config = hermes_agent::evolution_ledger::status_agent_config();
    hermes_agent::evolution_ledger::format_evolve_status(&home, &config)
}

fn dev_evolve_status(repo_root: &std::path::Path) -> String {
    let report_dir = repo_root.join(".sync-reports");
    latest_json_report(&report_dir, "self-evolution-loop-")
        .and_then(|p| summarize_self_evolution_report(&p, "self_evolution"))
        .unwrap_or_else(|| "self_evolution=unknown (no reports yet)".to_string())
}

pub(crate) async fn handle_ops_evolve_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match sub.as_str() {
        "status" => {
            let runtime = runtime_evolve_status();
            let dev = discover_repo_root_for_about()
                .map(|root| dev_evolve_status(&root))
                .unwrap_or_else(|| "self_evolution=unavailable (not in source checkout)".to_string());
            emit_command_output(
                host,
                format!(
                    "[runtime]\n{runtime}\n\n[dev]\n{dev}\n\nRun `/ops evolve run` for the dev self-evolution loop."
                ),
            );
            Ok(CommandResult::Handled)
        }
        "run" | "recommend" | "recs" => {
            let Some(repo_root) = discover_repo_root_for_about() else {
                emit_command_output(
                    host,
                    "Self-evolution run/recommend requires a source checkout. `/evolve status` still shows runtime ledger.",
                );
                return Ok(CommandResult::Handled);
            };
            let report_dir = repo_root.join(".sync-reports");
            if sub == "run" {
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
            } else {
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
            }
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(host, "Usage: /evolve [status|run|recommend]");
            Ok(CommandResult::Handled)
        }
    }
}
