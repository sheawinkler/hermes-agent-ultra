use hermes_core::AgentError;

use super::super::discover_repo_root_for_about;
use super::reports::{latest_json_report, summarize_gate_report};
use super::shell::{run_ops_shell_command, shell_escape};
use crate::commands::{CommandResult, emit_command_output};

pub(crate) async fn handle_ops_gate_command(
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
            if let Some(repo_root) = discover_repo_root_for_about() {
                let report_dir = repo_root.join(".sync-reports");
                let eval = latest_json_report(&report_dir, "eval-trend-gate-")
                    .and_then(|p| summarize_gate_report(&p, "eval_trend"))
                    .unwrap_or_else(|| "eval_trend=unknown".to_string());
                let slo = latest_json_report(&report_dir, "slo-auto-rollback-")
                    .and_then(|p| summarize_gate_report(&p, "slo_rollback"))
                    .unwrap_or_else(|| "slo_rollback=unknown".to_string());
                let elite = latest_json_report(&report_dir, "elite-sync-gate-")
                    .and_then(|p| summarize_gate_report(&p, "elite_sync_gate"))
                    .unwrap_or_else(|| "elite_sync_gate=unknown".to_string());
                emit_command_output(host, format!("{}\n{}\n{}", eval, slo, elite));
            } else {
                emit_command_output(host, "Gate status unavailable outside source checkout.");
            }
            Ok(CommandResult::Handled)
        }
        "eval" => {
            let out = run_ops_shell_command(
                "python3 scripts/run-eval-trend-gate.py --allow-missing-baseline --json",
            )
            .await?;
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        "elite" => {
            let out =
                run_ops_shell_command("python3 scripts/run-elite-sync-gate.py --json").await?;
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        "slo" => {
            let check_cmd = std::env::var("HERMES_SLO_CHECK_CMD").ok();
            let rollback_cmd = std::env::var("HERMES_SLO_ROLLBACK_CMD").ok();
            let (Some(check), Some(rollback)) = (check_cmd, rollback_cmd) else {
                emit_command_output(
                    host,
                    "Set HERMES_SLO_CHECK_CMD and HERMES_SLO_ROLLBACK_CMD, then run `/ops gate slo`.",
                );
                return Ok(CommandResult::Handled);
            };
            let cmd = format!(
                "python3 scripts/run-slo-auto-rollback.py --check-cmd {} --rollback-cmd {} --json",
                shell_escape(&check),
                shell_escape(&rollback)
            );
            let out = run_ops_shell_command(&cmd).await?;
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(host, "Usage: /ops gate [status|eval|elite|slo]");
            Ok(CommandResult::Handled)
        }
    }
}
