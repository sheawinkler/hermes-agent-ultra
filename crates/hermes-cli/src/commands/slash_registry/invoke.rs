use hermes_core::AgentError;

use super::SlashHandlerId;
use crate::acp_command;
use crate::commands::autocomplete::canonical_command;
use crate::commands::catalog::{
    handle_commands_catalog_command, handle_experiment_command, handle_feedback_command,
    handle_restart_command, handle_update_command, print_help,
};
use crate::commands::{CommandResult, emit_command_output};

pub async fn invoke_handler(
    id: SlashHandlerId,
    host: &mut (impl crate::app::SlashCommandHost + crate::app::AcpServerRuntime),
    original_cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    match id {
        SlashHandlerId::New => {
            host.new_session();
            let msg = if original_cmd.eq_ignore_ascii_case("/reset") {
                format!("[Session reset: {}]", host.session_id())
            } else {
                format!("[New session started: {}]", host.session_id())
            };
            emit_command_output(host, msg);
            Ok(CommandResult::Handled)
        }
        SlashHandlerId::Retry => {
            host.retry_last().await?;
            Ok(CommandResult::Handled)
        }
        SlashHandlerId::Undo => {
            host.undo_last();
            emit_command_output(host, "[Last exchange undone]");
            Ok(CommandResult::Handled)
        }
        SlashHandlerId::History => super::super::misc::handle_history_command(host),
        SlashHandlerId::Recap => super::super::misc::handle_recap_command(host, args),
        SlashHandlerId::Context => super::super::misc::handle_context_command(host, args).await,
        SlashHandlerId::Title => super::super::session::handle_session_compat_command(
            host,
            canonical_command(original_cmd),
            args,
        ),
        SlashHandlerId::Branch => super::super::session::handle_branch_command(host, args),
        SlashHandlerId::Timetravel => super::super::session::handle_timetravel_command(host, args),
        SlashHandlerId::Snapshot => super::super::session::handle_snapshot_command(host, args),
        SlashHandlerId::Rollback => super::super::session::handle_rollback_command(host, args),
        SlashHandlerId::Queue => super::super::background::handle_queue_command(host, args),
        SlashHandlerId::Handoff => super::super::objective::handle_handoff_command(host, args),
        SlashHandlerId::Steer => super::super::objective::handle_steer_command(host, args),
        SlashHandlerId::Btw => super::super::objective::handle_btw_command(host, args),
        SlashHandlerId::Subgoal => super::super::objective::handle_subgoal_command(host, args),
        SlashHandlerId::Sethome => super::super::objective::handle_sethome_command(host, args),
        SlashHandlerId::Evolve => super::super::ops::handle_ops_evolve_command(host, args).await,
        SlashHandlerId::Objective => super::super::objective::handle_objective_command(host, args),
        SlashHandlerId::Claims => super::super::claims::handle_claims_command(host, args),
        SlashHandlerId::Quorum => super::super::quorum::handle_quorum_command(host, args).await,
        SlashHandlerId::Swarm => super::super::swarm::handle_swarm_command(host, args).await,
        SlashHandlerId::Simulate => super::super::ops::handle_simulate_command(host, args),
        SlashHandlerId::Specpatch => {
            super::super::studio_ops::handle_specpatch_command(host, args).await
        }
        SlashHandlerId::Heatmap => {
            super::super::studio_ops::handle_heatmap_command(host, args).await
        }
        SlashHandlerId::Studio => super::super::studio_ops::handle_studio_command(host, args).await,
        SlashHandlerId::Ask => {
            super::super::diagnostics::handle_interactive_question_command(host, args)
        }
        SlashHandlerId::Model => super::super::model::handle_model_command(host, args).await,
        SlashHandlerId::Auth => super::super::auth_cmd::handle_auth_command(host, args).await,
        SlashHandlerId::Provider => super::super::misc::handle_provider_command(host).await,
        SlashHandlerId::Personality => super::super::misc::handle_personality_command(host, args),
        SlashHandlerId::Profile => super::super::runtime_ui::handle_profile_command(host),
        SlashHandlerId::Fast | SlashHandlerId::Skin | SlashHandlerId::Voice => {
            super::super::runtime_ui::handle_runtime_ui_mode_command(
                host,
                canonical_command(original_cmd),
                args,
            )
        }
        SlashHandlerId::Pet => super::super::runtime_ui::handle_pet_command(host, args),
        SlashHandlerId::Skills => super::super::skills::handle_skills_command(host, args).await,
        SlashHandlerId::Curator => super::super::misc::handle_curator_command(host, args).await,
        SlashHandlerId::Tools => super::super::misc::handle_tools_command(host, args),
        SlashHandlerId::Toolcards => super::super::misc::handle_toolcards_command(host, args),
        SlashHandlerId::Toolsets => super::super::infra::handle_toolsets_command(host),
        SlashHandlerId::Plugins => super::super::infra::handle_plugins_command(host),
        SlashHandlerId::Mcp => super::super::infra::handle_mcp_command(host),
        SlashHandlerId::Reload | SlashHandlerId::ReloadMcp => {
            super::super::infra::handle_reload_command(host, canonical_command(original_cmd))
        }
        SlashHandlerId::Cron => super::super::infra::handle_cron_command(host),
        SlashHandlerId::Agents => super::super::infra::handle_agents_command(host, args),
        SlashHandlerId::Kanban => super::super::kanban::handle_kanban_command(host, args),
        SlashHandlerId::Plan => super::super::plan::handle_plan_command(host, args),
        SlashHandlerId::PlanMode => {
            super::super::misc::handle_plan_mode_command(host, args).await
        }
        SlashHandlerId::Lsp => super::super::infra::handle_lsp_command(host, args),
        SlashHandlerId::Graph => super::super::infra::handle_graph_command(host, args).await,
        SlashHandlerId::Qos => super::super::ops::handle_qos_command(host, args).await,
        SlashHandlerId::Image => super::super::diagnostics::handle_image_command(host, args),
        SlashHandlerId::Config => super::super::misc::handle_config_command(host, args),
        SlashHandlerId::Autocompact => {
            super::super::compress::handle_autocompact_command(host, args).await
        }
        SlashHandlerId::Compress => {
            super::super::compress::handle_compress_command(host, args).await
        }
        SlashHandlerId::ClearQueue => super::super::background::handle_clear_queue_command(host),
        SlashHandlerId::Usage => super::super::misc::handle_usage_command(host),
        SlashHandlerId::Insights => super::super::diagnostics::handle_insights_command(host),
        SlashHandlerId::Stop => super::super::misc::handle_stop_command(host),
        SlashHandlerId::Status => super::super::misc::handle_status_command(host),
        SlashHandlerId::About => super::super::misc::handle_about_command(host),
        SlashHandlerId::Ops => super::super::ops::handle_ops_command(host, args).await,
        SlashHandlerId::Telemetry => super::super::auth_cmd::handle_telemetry_command(host, args),
        SlashHandlerId::Runbook => super::super::misc::handle_runbook_command(host, args),
        SlashHandlerId::Eval => super::super::ops::handle_ops_eval_command(host, args).await,
        SlashHandlerId::Autopilot => {
            super::super::ops::handle_ops_autopilot_command(host, args).await
        }
        SlashHandlerId::Mission => {
            super::super::background::handle_mission_command(host, args).await
        }
        SlashHandlerId::Dashboard => super::super::ops::handle_dashboard_command(host, args).await,
        SlashHandlerId::Platforms => super::super::integrations::handle_platforms_command(host),
        SlashHandlerId::Integrations => {
            super::super::integrations::handle_integrations_command(host, args).await
        }
        SlashHandlerId::Commands => handle_commands_catalog_command(host, args),
        SlashHandlerId::Boot => super::super::policy::handle_boot_command(host, args).await,
        SlashHandlerId::Walkthrough => super::super::policy::handle_walkthrough_command(host, args),
        SlashHandlerId::Triage => super::super::misc::handle_trigger_triage_command(host, args),
        SlashHandlerId::Subconscious => super::super::misc::handle_subconscious_command(host, args),
        SlashHandlerId::Log => super::super::diagnostics::handle_log_command(host),
        SlashHandlerId::DebugDump => {
            super::super::diagnostics::handle_debug_dump_command(host, args)
        }
        SlashHandlerId::DumpFormat => super::super::diagnostics::handle_dump_format_command(host),
        SlashHandlerId::Experiment => handle_experiment_command(host, args),
        SlashHandlerId::Feedback => handle_feedback_command(host, args),
        SlashHandlerId::Restart => handle_restart_command(host, args),
        SlashHandlerId::Update => handle_update_command(host, args).await,
        SlashHandlerId::Redraw => super::super::runtime_ui::handle_redraw_command(host),
        SlashHandlerId::Paste => super::super::runtime_ui::handle_paste_command(host, args),
        SlashHandlerId::Gquota => super::super::approval::handle_gquota_command(host, args).await,
        SlashHandlerId::Approve => super::super::approval::handle_approve_command(host, args),
        SlashHandlerId::Deny => super::super::approval::handle_deny_command(host, args),
        SlashHandlerId::Copy => super::super::runtime_ui::handle_copy_command(host),
        SlashHandlerId::Save => super::super::session::handle_save_command(host, args),
        SlashHandlerId::Load => super::super::session::handle_load_command(host, args),
        SlashHandlerId::Resume => super::super::session::handle_resume_command(host, args),
        SlashHandlerId::Sessions => super::super::session::handle_sessions_command(host, args),
        SlashHandlerId::Background => {
            super::super::background::handle_background_command(host, args)
        }
        SlashHandlerId::Mouse => super::super::runtime_ui::handle_mouse_command(host, args),
        SlashHandlerId::Verbose => super::super::misc::handle_verbose_command(host),
        SlashHandlerId::Statusbar => super::super::runtime_ui::handle_statusbar_command(host),
        SlashHandlerId::Yolo => super::super::misc::handle_yolo_command(host),
        SlashHandlerId::Browser => super::super::browser::handle_browser_command(host, args).await,
        SlashHandlerId::Reasoning => super::super::misc::handle_reasoning_command(host, args),
        SlashHandlerId::Raw => super::super::misc::handle_raw_command(host, args),
        SlashHandlerId::Policy => super::super::policy::handle_policy_command(host, args),
        SlashHandlerId::Help => {
            print_help(host);
            Ok(CommandResult::Handled)
        }
        SlashHandlerId::AcpServer => acp_command::handle_acp_command(host, args).await,
        SlashHandlerId::Quit => {
            emit_command_output(host, "Goodbye!");
            Ok(CommandResult::Quit)
        }
    }
}
