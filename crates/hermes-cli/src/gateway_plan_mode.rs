//! Plan mode wiring for messaging gateway sessions (WeCom, Weixin, Telegram, etc.).

use std::sync::Arc;

use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_gateway::gateway::IncomingMessage;
use hermes_gateway::{Gateway, GatewayError};
use hermes_tools::PlanPhase;

use crate::app::bridge_tool_registry;
use crate::plan_mode::{
    PlanApprovalParseStyle, PlanModeSlashAction, PlanTurnPrep, finalize_plan_agent_reply,
    parse_plan_mode_slash_args, plan_mode_help_text, plan_mode_status_text, prepare_plan_turn,
};

use crate::gateway_handlers::GatewayHandlerDeps;
use crate::gateway_main::get_or_build_gateway_cached_agent;

pub fn prepare_gateway_plan_turn(
    agent: &hermes_agent::AgentLoop,
    user_message: &str,
) -> PlanTurnPrep {
    prepare_plan_turn(agent, user_message, PlanApprovalParseStyle::Channel)
}

pub fn finalize_gateway_agent_reply(
    agent: &hermes_agent::AgentLoop,
    conv: &hermes_agent::ConversationResult,
) -> String {
    finalize_plan_agent_reply(agent, conv)
}

async fn gateway_agent_for_session(
    gateway: &Gateway,
    incoming: &IncomingMessage,
    session_key: &str,
    deps: &GatewayHandlerDeps,
) -> Result<Arc<tokio::sync::Mutex<hermes_agent::AgentLoop>>, GatewayError> {
    let ctx = gateway
        .runtime_context_for_route(incoming, session_key)
        .await;
    let agent_tools: Arc<AgentToolRegistry> = Arc::new(bridge_tool_registry(&deps.runtime_tools));
    Ok(get_or_build_gateway_cached_agent(
        &deps.gateway_agent_cache,
        deps.config.as_ref(),
        &ctx,
        agent_tools,
        deps.runtime_tools.clone(),
    )
    .await)
}

/// Handle `/plan-mode` from a messaging channel (WeCom, Weixin, etc.).
pub async fn execute_plan_mode_slash_command(
    gateway: Arc<Gateway>,
    incoming: &IncomingMessage,
    session_key: &str,
    args: &str,
    deps: GatewayHandlerDeps,
) -> Result<(), GatewayError> {
    let action = parse_plan_mode_slash_args(args);
    let agent_arc = gateway_agent_for_session(&gateway, incoming, session_key, &deps).await?;
    let agent = agent_arc.lock().await;

    match action {
        PlanModeSlashAction::Help => {
            gateway
                .send_incoming_reply(incoming, plan_mode_help_text(), None)
                .await?;
        }
        PlanModeSlashAction::On => {
            agent.set_plan_phase(PlanPhase::Planning);
            gateway
                .send_incoming_reply(
                    incoming,
                    "Plan mode ON: agent will research with read-only tools, submit a plan, and wait for approval.",
                    None,
                )
                .await?;
        }
        PlanModeSlashAction::Off => {
            agent.set_plan_phase(PlanPhase::Off);
            agent.set_pending_plan(None);
            gateway
                .send_incoming_reply(incoming, "Plan mode OFF.", None)
                .await?;
        }
        PlanModeSlashAction::Status => {
            let text = plan_mode_status_text(&agent);
            gateway.send_incoming_reply(incoming, &text, None).await?;
        }
        PlanModeSlashAction::Approve => {
            if agent.plan_phase() != PlanPhase::AwaitingApproval {
                gateway
                    .send_incoming_reply(
                        incoming,
                        "No plan awaiting approval. Use /plan-mode <task> first.",
                        None,
                    )
                    .await?;
                return Ok(());
            }
            agent.set_plan_phase(PlanPhase::Executing);
            drop(agent);
            gateway
                .append_user_message_and_route(
                    incoming,
                    session_key,
                    "Plan approved. Proceed with execution.".to_string(),
                )
                .await?;
        }
        PlanModeSlashAction::Reject { feedback } => {
            agent.set_plan_phase(PlanPhase::Planning);
            agent.set_pending_plan(None);
            drop(agent);
            if feedback.trim().is_empty() {
                gateway
                    .send_incoming_reply(
                        incoming,
                        "Plan rejected. Revise your request or send a new task with /plan-mode.",
                        None,
                    )
                    .await?;
            } else {
                gateway
                    .append_user_message_and_route(
                        incoming,
                        session_key,
                        format!("Plan rejected. User feedback: {feedback}"),
                    )
                    .await?;
            }
        }
        PlanModeSlashAction::Edit { plan } => {
            if plan.trim().is_empty() {
                gateway
                    .send_incoming_reply(
                        incoming,
                        "Usage: /plan-mode edit <revised plan text>",
                        None,
                    )
                    .await?;
                return Ok(());
            }
            agent.set_pending_plan(Some(plan.clone()));
            agent.set_plan_phase(PlanPhase::Executing);
            drop(agent);
            gateway
                .append_user_message_and_route(
                    incoming,
                    session_key,
                    format!("Plan updated and approved:\n{plan}"),
                )
                .await?;
        }
        PlanModeSlashAction::Task { task } => {
            if task.trim().is_empty() {
                gateway
                    .send_incoming_reply(incoming, plan_mode_help_text(), None)
                    .await?;
                return Ok(());
            }
            agent.set_plan_phase(PlanPhase::Planning);
            drop(agent);
            gateway
                .append_user_message_and_route(incoming, session_key, task)
                .await?;
        }
    }
    Ok(())
}
