use std::fmt::Write as _;

use hermes_core::AgentError;

use super::super::{
    background, compress, discover_repo_root_for_about, plan::plan_capability_mode, policy,
    read_json_file, replay_enabled_runtime, session,
};
use super::budget::RepoReviewBudgetRuntime;
use super::reports::{latest_json_report, summarize_gate_report};
use super::route::{
    route_health_state_path, route_learning_state_path, summarize_route_health_state,
};
use crate::alpha_runtime::render_mission_board;
use crate::commands::{CommandResult, emit_command_output};

pub(crate) async fn handle_ops_cockpit_command(
    host: &mut impl crate::app::SlashCommandHost,
    _args: &[&str],
) -> Result<CommandResult, AgentError> {
    let counters = host.tool_registry().policy_counters();
    let budget = RepoReviewBudgetRuntime::from_env();
    let board = render_mission_board(
        host.current_model(),
        host.session_objective(),
        background::background_job_counts(),
    )
    .await?;
    let route_health = summarize_route_health_state(&route_health_state_path());
    let eval_summary = if let Some(repo_root) = discover_repo_root_for_about() {
        let report_dir = repo_root.join(".sync-reports");
        latest_json_report(&report_dir, "session-eval-harness-")
            .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"))
            .and_then(|p| summarize_gate_report(&p, "eval"))
            .unwrap_or_else(|| "eval=unknown".to_string())
    } else {
        "eval=unavailable".to_string()
    };
    let snapshot_count =
        session::enumerate_saved_sessions(&hermes_config::hermes_home().join("sessions")).len();
    let mut out = String::new();
    out.push_str("Ops Cockpit\n");
    out.push_str("===========\n");
    let _ = writeln!(out, "session: {}", host.session_id());
    let _ = writeln!(out, "model: {}", host.current_model());
    let _ = writeln!(
        out,
        "policy: profile={} mode={} preset={} sandbox={} skills_tier={}",
        policy::current_policy_profile_name(),
        std::env::var("HERMES_TOOL_POLICY_MODE").unwrap_or_else(|_| "enforce".into()),
        std::env::var("HERMES_TOOL_POLICY_PRESET").unwrap_or_else(|_| "relaxed".into()),
        std::env::var("HERMES_EXECUTION_SANDBOX_PROFILE").unwrap_or_else(|_| "balanced".into()),
        std::env::var("HERMES_SKILLS_EXECUTION_TIER").unwrap_or_else(|_| "balanced".into())
    );
    let _ = writeln!(
        out,
        "planner_capability_router={} compaction_governance={} replay_trace={}",
        plan_capability_mode().as_str(),
        compress::compaction_governance_mode().as_str(),
        if replay_enabled_runtime() {
            "on"
        } else {
            "off"
        }
    );
    let _ = writeln!(
        out,
        "repo_review_budget: profile={} repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2}",
        budget.profile.as_str(),
        budget.repeat_threshold,
        budget.low_signal_threshold,
        budget.keep_repeat,
        budget.keep_low_signal,
        budget.min_signal_score
    );
    let _ = writeln!(
        out,
        "policy_counters: allow={} deny={} audit_only={} simulate={} would_block={}",
        counters.allow, counters.deny, counters.audit_only, counters.simulate, counters.would_block
    );
    let _ = writeln!(
        out,
        "qos: {} | learning_entries={} | snapshots={}",
        route_health,
        read_json_file(&route_learning_state_path())
            .and_then(|v| v
                .get("entries")
                .and_then(|e| e.as_array())
                .map(|arr| arr.len()))
            .unwrap_or(0usize),
        snapshot_count
    );
    let _ = writeln!(out, "eval: {}", eval_summary);
    out.push('\n');
    out.push_str(&board);
    emit_command_output(host, out.trim_end());
    Ok(CommandResult::Handled)
}
