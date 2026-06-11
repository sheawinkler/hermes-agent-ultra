use hermes_core::AgentError;

use super::super::discover_repo_root_for_about;
use super::reports::{
    latest_json_report, performance_autopilot_recommendations,
    summarize_performance_autopilot_report,
};
use super::shell::{
    parse_env_file_kv, run_ops_shell_command, shell_escape, write_autopilot_runtime_event,
};
use crate::commands::{CommandResult, emit_command_output};

const AUTOPILOT_ALLOWED_ENV_KEYS: &[&str] = &[
    "HERMES_TOOL_POLICY_PRESET",
    "HERMES_TOOL_POLICY_MODE",
    "HERMES_MODEL_CATALOG_GUARD",
    "HERMES_MODEL_AUTO_REMEDIATE",
    "HERMES_REPLAY_ENABLED",
    "HERMES_PERF_AUTOPILOT_STATUS",
    "HERMES_PERF_AUTOPILOT_PROFILE",
    "HERMES_PERF_AUTOPILOT_MODE",
];

pub(crate) async fn handle_ops_autopilot_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "advisory".to_string());
    let profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());

    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            host,
            "Autopilot controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    let latest = latest_json_report(&report_dir, "performance-autopilot-");

    match sub.as_str() {
        "status" => {
            let summary = latest
                .as_ref()
                .and_then(|p| summarize_performance_autopilot_report(p, "autopilot"))
                .unwrap_or_else(|| "autopilot=unknown (no reports yet)".to_string());
            emit_command_output(
                host,
                format!(
                    "{}\nmode={} profile={}\nUse `/ops autopilot run` then `/ops autopilot recommend`.",
                    summary, mode, profile
                ),
            );
            Ok(CommandResult::Handled)
        }
        "run" => {
            let out = run_ops_shell_command(
                "python3 scripts/run-performance-autopilot.py --repo-root . --json",
            )
            .await?;
            emit_command_output(host, out);
            Ok(CommandResult::Handled)
        }
        "recommend" | "recs" => {
            let Some(path) = latest else {
                emit_command_output(
                    host,
                    "No performance autopilot reports found. Run `/ops autopilot run` first.",
                );
                return Ok(CommandResult::Handled);
            };
            let recs = performance_autopilot_recommendations(&path);
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
                        "Autopilot recommendations ({file_label}):\n{}",
                        recs.join("\n")
                    ),
                );
            }
            Ok(CommandResult::Handled)
        }
        "apply" => {
            let env_path = report_dir.join(format!(
                "performance-autopilot-env-{}.env",
                host.session_id()
            ));
            let cmd = format!(
                "python3 scripts/run-performance-autopilot.py --repo-root . --apply-env {} --json",
                shell_escape(&env_path.display().to_string())
            );
            let out = run_ops_shell_command(&cmd).await?;
            let kvs = parse_env_file_kv(&env_path);
            let mut applied = Vec::new();
            for (k, v) in kvs {
                if AUTOPILOT_ALLOWED_ENV_KEYS
                    .iter()
                    .any(|allowed| *allowed == k)
                {
                    crate::env_vars::set_var(&k, &v);
                    applied.push((k, v));
                }
            }
            write_autopilot_runtime_event(
                &report_dir,
                host.session_id(),
                &mode,
                &profile,
                &applied,
            );
            let applied_keys = if applied.is_empty() {
                "(none)".to_string()
            } else {
                applied
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            emit_command_output(
                host,
                format!(
                    "{out}\n\nApplied safe runtime knobs: {applied_keys}\nmode={mode} profile={profile}\nlog: {}",
                    report_dir
                        .join("performance-autopilot-runtime.jsonl")
                        .display()
                ),
            );
            Ok(CommandResult::Handled)
        }
        "profile" => {
            let next = args.get(1).map(|v| v.to_ascii_lowercase());
            match next.as_deref() {
                None | Some("status") => {
                    emit_command_output(host, format!("autopilot profile={profile} (mode={mode})"))
                }
                Some("list") => emit_command_output(
                    host,
                    "Autopilot profiles:\n- balanced: default stability/perf mix\n- throughput: lower latency and tighter loop cadence\n- quality: stronger verification and replay focus\n- reliability: prioritize retries/recovery and degraded-source tolerance\n- safety: strictest gate posture with conservative policy knobs",
                ),
                Some("balanced" | "throughput" | "quality" | "reliability" | "safety") => {
                    let value = next.unwrap_or_else(|| "off".to_string());
                    crate::env_vars::set_var("HERMES_PERF_AUTOPILOT_PROFILE", &value);
                    emit_command_output(host, format!("autopilot profile set to '{}'", value));
                }
                Some(other) => {
                    emit_command_output(
                        host,
                        format!(
                            "Unknown profile '{}'. Use `/ops autopilot profile list`.",
                            other
                        ),
                    );
                }
            }
            Ok(CommandResult::Handled)
        }
        "mode" => {
            let next = args.get(1).map(|v| v.to_ascii_lowercase());
            match next.as_deref() {
                None | Some("status") => {
                    emit_command_output(host, format!("autopilot mode={mode}"))
                }
                Some("list") => emit_command_output(
                    host,
                    "Autopilot modes:\n- off: disabled\n- advisory: report + recommendations only\n- enforce: intended to pair with `/ops autopilot apply` during incidents",
                ),
                Some("off" | "advisory" | "enforce") => {
                    let value = next.unwrap_or_else(|| "advisory".to_string());
                    crate::env_vars::set_var("HERMES_PERF_AUTOPILOT_MODE", &value);
                    emit_command_output(host, format!("autopilot mode set to '{}'", value));
                }
                Some(other) => {
                    emit_command_output(
                        host,
                        format!("Unknown mode '{}'. Use `/ops autopilot mode list`.", other),
                    );
                }
            }
            Ok(CommandResult::Handled)
        }
        "clear" => {
            crate::env_vars::remove_var("HERMES_PERF_AUTOPILOT_MODE");
            crate::env_vars::remove_var("HERMES_PERF_AUTOPILOT_PROFILE");
            crate::env_vars::remove_var("HERMES_PERF_AUTOPILOT_STATUS");
            emit_command_output(
                host,
                "Cleared autopilot runtime overrides (mode/profile/status).",
            );
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(
                host,
                "Usage: /ops autopilot [status|run|recommend|apply|profile [status|list|balanced|throughput|quality|reliability|safety]|mode [status|list|off|advisory|enforce]|clear]",
            );
            Ok(CommandResult::Handled)
        }
    }
}
