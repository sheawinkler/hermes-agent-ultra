//! Slash-command dispatch router.

use hermes_core::AgentError;

use super::autocomplete::{canonical_command, expand_quick_alias_command};
use super::catalog::{
    handle_commands_catalog_command, handle_experiment_command, handle_feedback_command,
    handle_restart_command, handle_update_command, print_help,
};
use super::{CommandResult, emit_command_output};
use crate::app::App;
use crate::app::traits::{ModelRuntime, SessionRuntime, SessionRuntimeAsync};

/// Handle a slash command.
///
/// `cmd` is the full command token including the `/` prefix
/// (e.g. `/model`, `/new`). `args` are the remaining tokens.
pub async fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let (resolved_cmd, arg_storage) =
        match expand_quick_alias_command(&app.config().quick_commands, cmd, args) {
            Ok(expanded) => expanded,
            Err(message) => {
                emit_command_output(app, message);
                return Ok(CommandResult::Handled);
            }
        };
    let arg_refs: Vec<&str> = arg_storage.iter().map(|part| part.as_str()).collect();
    let args = arg_refs.as_slice();
    let cmd = resolved_cmd.as_str();
    match canonical_command(cmd) {
        "/new" => {
            app.new_session();
            let msg = if cmd.eq_ignore_ascii_case("/reset") {
                format!("[Session reset: {}]", app.session_id())
            } else {
                format!("[New session started: {}]", app.session_id())
            };
            emit_command_output(app, msg);
            Ok(CommandResult::Handled)
        }
        "/retry" => {
            app.retry_last().await?;
            Ok(CommandResult::Handled)
        }
        "/undo" => {
            app.undo_last();
            emit_command_output(app, "[Last exchange undone]");
            Ok(CommandResult::Handled)
        }
        "/history" => super::misc::handle_history_command(app),
        "/recap" => super::misc::handle_recap_command(app, args),
        "/context" => super::misc::handle_context_command(app, args).await,
        "/title" => {
            super::session::handle_session_compat_command(app, canonical_command(cmd), args)
        }
        "/branch" => super::session::handle_branch_command(app, args),
        "/timetravel" => super::session::handle_timetravel_command(app, args),
        "/snapshot" => super::session::handle_snapshot_command(app, args),
        "/rollback" => super::session::handle_rollback_command(app, args),
        "/queue" => super::background::handle_queue_command(app, args),
        "/handoff" => super::objective::handle_handoff_command(app, args),
        "/steer" => super::objective::handle_steer_command(app, args),
        "/btw" => super::objective::handle_btw_command(app, args),
        "/subgoal" => super::objective::handle_subgoal_command(app, args),
        "/sethome" => super::objective::handle_sethome_command(app, args),
        "/evolve" => super::ops::handle_ops_evolve_command(app, args).await,
        "/objective" => super::objective::handle_objective_command(app, args),
        "/claims" => super::claims::handle_claims_command(app, args),
        "/quorum" => super::quorum::handle_quorum_command(app, args).await,
        "/swarm" => super::swarm::handle_swarm_command(app, args).await,
        "/simulate" => super::ops::handle_simulate_command(app, args),
        "/specpatch" => super::studio_ops::handle_specpatch_command(app, args).await,
        "/heatmap" => super::studio_ops::handle_heatmap_command(app, args).await,
        "/studio" => super::studio_ops::handle_studio_command(app, args).await,
        "/ask" => super::diagnostics::handle_interactive_question_command(app, args),
        "/model" => super::model::handle_model_command(app, args).await,
        "/auth" => super::auth_cmd::handle_auth_command(app, args).await,
        "/provider" => super::misc::handle_provider_command(app).await,
        "/personality" => super::misc::handle_personality_command(app, args),
        "/profile" | "/whoami" => super::runtime_ui::handle_profile_command(app),
        "/fast" | "/skin" | "/voice" => {
            super::runtime_ui::handle_runtime_ui_mode_command(app, canonical_command(cmd), args)
        }
        "/pet" => super::runtime_ui::handle_pet_command(app, args),
        "/skills" => super::skills::handle_skills_command(app, args).await,
        "/curator" => super::misc::handle_curator_command(app, args).await,
        "/tools" => super::misc::handle_tools_command(app, args),
        "/toolcards" => super::misc::handle_toolcards_command(app, args),
        "/toolsets" => super::infra::handle_toolsets_command(app),
        "/plugins" => super::infra::handle_plugins_command(app),
        "/mcp" => super::infra::handle_mcp_command(app),
        "/reload" | "/reload-mcp" => {
            super::infra::handle_reload_command(app, canonical_command(cmd))
        }
        "/cron" => super::infra::handle_cron_command(app),
        "/agents" => super::infra::handle_agents_command(app, args),
        "/kanban" => super::kanban::handle_kanban_command(app, args),
        "/plan" => super::plan::handle_plan_command(app, args),
        "/lsp" => super::infra::handle_lsp_command(app, args),
        "/graph" => super::infra::handle_graph_command(app, args).await,
        "/qos" => super::ops::handle_qos_command(app, args).await,
        "/image" => super::diagnostics::handle_image_command(app, args),
        "/config" => super::misc::handle_config_command(app, args),
        "/autocompact" => super::compress::handle_autocompact_command(app, args).await,
        "/compress" => super::compress::handle_compress_command(app, args).await,
        "/clear-queue" => super::background::handle_clear_queue_command(app),
        "/usage" => super::misc::handle_usage_command(app),
        "/insights" => super::diagnostics::handle_insights_command(app),
        "/stop" => super::misc::handle_stop_command(app),
        "/status" => super::misc::handle_status_command(app),
        "/about" => super::misc::handle_about_command(app),
        "/ops" => super::ops::handle_ops_command(app, args).await,
        "/telemetry" => super::auth_cmd::handle_telemetry_command(app, args),
        "/runbook" => super::misc::handle_runbook_command(app, args),
        "/eval" => super::ops::handle_ops_eval_command(app, args).await,
        "/autopilot" => super::ops::handle_ops_autopilot_command(app, args).await,
        "/mission" => super::background::handle_mission_command(app, args).await,
        "/dashboard" => super::ops::handle_dashboard_command(app, args).await,
        "/platforms" => super::integrations::handle_platforms_command(app),
        "/integrations" => super::integrations::handle_integrations_command(app, args).await,
        "/commands" => handle_commands_catalog_command(app, args),
        "/boot" => super::policy::handle_boot_command(app, args).await,
        "/walkthrough" => super::policy::handle_walkthrough_command(app, args),
        "/triage" => super::misc::handle_trigger_triage_command(app, args),
        "/subconscious" => super::misc::handle_subconscious_command(app, args),
        "/log" => super::diagnostics::handle_log_command(app),
        "/debug-dump" => super::diagnostics::handle_debug_dump_command(app, args),
        "/dump-format" => super::diagnostics::handle_dump_format_command(app),
        "/experiment" => handle_experiment_command(app, args),
        "/feedback" => handle_feedback_command(app, args),
        "/restart" => handle_restart_command(app, args),
        "/update" => handle_update_command(app, args).await,
        "/redraw" => super::runtime_ui::handle_redraw_command(app),
        "/paste" => super::runtime_ui::handle_paste_command(app, args),
        "/gquota" => super::approval::handle_gquota_command(app, args).await,
        "/approve" => super::approval::handle_approve_command(app, args),
        "/deny" => super::approval::handle_deny_command(app, args),
        "/copy" => super::runtime_ui::handle_copy_command(app),
        "/save" => super::session::handle_save_command(app, args),
        "/load" => super::session::handle_load_command(app, args),
        "/resume" => super::session::handle_resume_command(app, args),
        "/sessions" => super::session::handle_sessions_command(app, args),
        "/background" => super::background::handle_background_command(app, args),
        "/mouse" => super::runtime_ui::handle_mouse_command(app, args),
        "/verbose" => super::misc::handle_verbose_command(app),
        "/statusbar" => super::runtime_ui::handle_statusbar_command(app),
        "/yolo" => super::misc::handle_yolo_command(app),
        "/browser" => super::browser::handle_browser_command(app, args).await,
        "/reasoning" => super::misc::handle_reasoning_command(app, args),
        "/raw" => super::misc::handle_raw_command(app, args),
        "/policy" => super::policy::handle_policy_command(app, args),
        "/help" => {
            print_help(app);
            Ok(CommandResult::Handled)
        }
        "/acp_server" => crate::acp_command::handle_acp_command(app, args).await,
        "/quit" | "/exit" => {
            emit_command_output(app, "Goodbye!");
            Ok(CommandResult::Quit)
        }
        _ => {
            emit_command_output(
                app,
                format!(
                    "Unknown command: {}. Type /help for available commands.",
                    cmd
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}
