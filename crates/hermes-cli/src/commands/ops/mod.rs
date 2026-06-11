//! OPS commands — operator control plane, dashboards, simulation, autopilot,
//! self-evolution, gate, QoS routing, task-depth profiles, and budget controls.

mod autopilot;
mod budget;
mod cockpit;
mod dashboard;
mod eval;
mod evolve;
mod gate;
mod qos;
mod reports;
mod route;
mod shell;
mod simulate;
mod skills_tier;
mod task_depth;
mod tool_profile;

#[cfg(test)]
mod tests;

use hermes_core::AgentError;

use super::{discover_repo_root_for_about, policy, skills};
use crate::commands::{CommandResult, emit_command_output};

pub(crate) use autopilot::handle_ops_autopilot_command;
pub(crate) use dashboard::handle_dashboard_command;
pub(crate) use eval::handle_ops_eval_command;
pub(crate) use evolve::handle_ops_evolve_command;
pub(crate) use qos::handle_qos_command;
pub(crate) use reports::{
    latest_json_report, summarize_gate_report, summarize_performance_autopilot_report,
};
pub(crate) use simulate::handle_simulate_command;
pub(crate) use task_depth::{
    TaskDepthProfile, apply_task_depth_profile, task_depth_runtime_summary,
};

use budget::{RepoReviewBudgetRuntime, handle_ops_budget_command};
use reports::summarize_self_evolution_report;
use shell::dashboard_status_line_from_payload;
use skills_tier::handle_ops_skills_tier_command;
use tool_profile::handle_ops_tool_profile_command;

pub(crate) async fn handle_ops_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let yolo = !host.config().approval.require_approval;
        let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "enforce".to_string());
        let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());
        let counters = host.tool_registry().policy_counters();
        let dashboard_status = {
            let raw = host
                .tool_registry()
                .dispatch_async("dashboard_control", serde_json::json!({"action":"status"}))
                .await;
            let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(
                |_| serde_json::json!({"enabled":false,"url":"unknown","error":"unparseable"}),
            );
            dashboard_status_line_from_payload(&parsed)
        };
        let gate_status = if let Some(repo_root) = discover_repo_root_for_about() {
            let report_dir = repo_root.join(".sync-reports");
            let eval = latest_json_report(&report_dir, "eval-trend-gate-")
                .and_then(|p| summarize_gate_report(&p, "eval"))
                .unwrap_or_else(|| "eval=unknown".to_string());
            let slo = latest_json_report(&report_dir, "slo-auto-rollback-")
                .and_then(|p| summarize_gate_report(&p, "slo"))
                .unwrap_or_else(|| "slo=unknown".to_string());
            let evolve = latest_json_report(&report_dir, "self-evolution-loop-")
                .and_then(|p| summarize_self_evolution_report(&p, "evolve"))
                .unwrap_or_else(|| "evolve=unknown".to_string());
            let autopilot = latest_json_report(&report_dir, "performance-autopilot-")
                .and_then(|p| summarize_performance_autopilot_report(&p, "autopilot"))
                .unwrap_or_else(|| "autopilot=unknown".to_string());
            format!("{eval}; {slo}; {evolve}; {autopilot}")
        } else {
            "unavailable (non-source checkout)".to_string()
        };
        let autopilot_mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "advisory".to_string());
        let autopilot_profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());
        let repo_review_budget = RepoReviewBudgetRuntime::from_env();
        let tool_profile_mode = std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());

        let out = format!(
            "Operator Control Plane\n\
             \n\
             Runtime:\n\
               session:      {}\n\
               model:        {}\n\
               personality:  {}\n\
             \n\
             Controls:\n\
               yolo:         {}\n\
               mouse:        {}\n\
               statusbar:    ON\n\
               reasoning:    `/ops reasoning status` + `/ops reasoning set ...`\n\
               raw:          toggle via `/ops raw`\n\
               verbose:      toggle via `/ops verbose`\n\
             \n\
             Policy/Gates:\n\
               tool_policy:  mode={} preset={}\n\
               autopilot:    mode={} profile={}\n\
               tool_profile: {}\n\
               repo_budget:  profile={} repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2}\n\
               task_depth:   {}\n\
               policy_counts allow={} deny={} audit_only={} simulate={} would_block={}\n\
               skills_tier:  {} (bypass={})\n\
               {}\n\
               gate_status:  {}\n\
             \n\
             Quick actions:\n\
               /ops model [provider|provider:model]\n\
               /ops mode [status|list|strict|standard|dev]\n\
               /ops personality [list|name]\n\
               /ops mouse [on|off|toggle]\n\
               /ops yolo\n\
               /ops reasoning [status|on|off|toggle|set <level>]\n\
               /ops raw [on|off|toggle|once|trace ...]\n\
               /ops verbose\n\
               /ops dashboard [status|on|off|url] [host] [port]\n\
               /ops skills-tier [status|trusted|balanced|open]\n\
               /ops tool-profile [status|list|off|balanced|focus]\n\
               /ops budget [status|list|balanced|aggressive|relaxed|off|clear]\n\
               /ops evolve [status|run|recommend]\n\
               /ops eval [status|run|latest]\n\
               /ops autopilot [status|run|recommend|apply|profile|mode|clear]\n\
               /ops gate [status|eval|elite|slo]\n\
               /ops cockpit\n\
               /mission [status|init]\n\
               /ops help",
            host.session_id(),
            host.current_model(),
            host.current_personality().unwrap_or("(none)"),
            if yolo { "ON" } else { "OFF" },
            if host.mouse_enabled() { "ON" } else { "OFF" },
            policy_mode,
            policy_preset,
            autopilot_mode,
            autopilot_profile,
            tool_profile_mode,
            repo_review_budget.profile.as_str(),
            repo_review_budget.repeat_threshold,
            repo_review_budget.low_signal_threshold,
            repo_review_budget.keep_repeat,
            repo_review_budget.keep_low_signal,
            repo_review_budget.min_signal_score,
            task_depth_runtime_summary(),
            counters.allow,
            counters.deny,
            counters.audit_only,
            counters.simulate,
            counters.would_block,
            skills::skills_execution_tier().as_str(),
            if skills::skills_tier_bypass_enabled() {
                "ON"
            } else {
                "OFF"
            },
            dashboard_status,
            gate_status,
        );
        emit_command_output(host, out);
        return Ok(CommandResult::Handled);
    }

    match args[0].to_ascii_lowercase().as_str() {
        "help" => {
            emit_command_output(
                host,
                "Operator control plane commands:\n\
                 - /ops status\n\
                 - /ops model [provider|provider:model]\n\
                 - /ops mode [status|list|strict|standard|dev]\n\
                 - /ops personality [list|name]\n\
                 - /ops mouse [on|off|toggle]\n\
                 - /ops yolo\n\
                 - /ops reasoning [status|on|off|toggle|set <level>]\n\
                 - /ops raw [on|off|toggle|once|trace ...]\n\
                 - /ops verbose\n\
                 - /ops statusbar\n\
                 - /ops dashboard [status|on|off|url] [host] [port]\n\
                 - /ops skills-tier [status|trusted|balanced|open]\n\
                 - /ops tool-profile [status|list|off|balanced|focus]\n\
                 - /ops budget [status|list|balanced|aggressive|relaxed|off|clear]\n\
                 - /ops evolve [status|run|recommend]\n\
                 - /ops eval [status|run|latest]\n\
                 - /ops autopilot [status|run|recommend|apply|profile|mode|clear]\n\
                 - /ops gate [status|eval|elite|slo]\n\
                 - /ops cockpit\n\
                 - /mission [status|init]",
            );
            Ok(CommandResult::Handled)
        }
        "model" => super::model::handle_model_command(host, &args[1..]).await,
        "mode" => policy::handle_policy_command(host, &args[1..]),
        "personality" => super::handle_personality_command(host, &args[1..]),
        "mouse" => super::runtime_ui::handle_mouse_command(host, &args[1..]),
        "yolo" => super::handle_yolo_command(host),
        "reasoning" => super::handle_reasoning_command(host, &args[1..]),
        "raw" => super::handle_raw_command(host, &args[1..]),
        "verbose" => super::handle_verbose_command(host),
        "statusbar" => super::runtime_ui::handle_statusbar_command(host),
        "dashboard" => handle_dashboard_command(host, &args[1..]).await,
        "skills-tier" => handle_ops_skills_tier_command(host, &args[1..]),
        "tool-profile" | "toolprofile" | "tool_profile" => {
            handle_ops_tool_profile_command(host, &args[1..])
        }
        "budget" => handle_ops_budget_command(host, &args[1..]),
        "evolve" => handle_ops_evolve_command(host, &args[1..]).await,
        "eval" => handle_ops_eval_command(host, &args[1..]).await,
        "autopilot" => handle_ops_autopilot_command(host, &args[1..]).await,
        "gate" => gate::handle_ops_gate_command(host, &args[1..]).await,
        "cockpit" => cockpit::handle_ops_cockpit_command(host, &args[1..]).await,
        other => {
            emit_command_output(
                host,
                format!(
                    "Unknown /ops target '{}'. Try `/ops help` for available controls.",
                    other
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}
