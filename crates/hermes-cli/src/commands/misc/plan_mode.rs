//! `/plan-mode` slash command handler.

use hermes_core::{AgentError, Message};
use hermes_tools::PlanPhase;

use crate::commands::{CommandResult, emit_command_output};

const PLAN_MODE_SUBCOMMANDS: &[&str] = &[
    "on", "enable", "off", "disable", "status", "show", "approve", "accept", "a", "reject",
    "deny", "r", "edit", "e", "help", "usage",
];

fn is_plan_mode_subcommand(word: &str) -> bool {
    PLAN_MODE_SUBCOMMANDS.contains(&word.to_ascii_lowercase().as_str())
}

async fn plan_mode_run_task(
    host: &mut impl crate::app::SlashCommandHost,
    task: &str,
) -> Result<(), AgentError> {
    host.agent().set_plan_phase(PlanPhase::Planning);
    host.messages_mut()
        .push(Message::user(task.to_string()));
    host.run_agent_turn().await
}

pub(crate) async fn handle_plan_mode_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty()
        || args
            .first()
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "help" | "usage"))
    {
        emit_command_output(
            host,
            "Plan mode (plan-then-execute):\n\
             /plan-mode <task>      Start planning for <task> (read-only until approved)\n\
             /plan-mode on          Enable planning phase without sending a task\n\
             /plan-mode off         Disable plan mode\n\
             /plan-mode status      Show current phase\n\
             /plan-mode approve     Approve pending plan and execute\n\
             /plan-mode reject [feedback]  Reject plan and return to planning\n\
             /plan-mode edit <text> Revise plan, approve, and execute",
        );
        return Ok(CommandResult::Handled);
    }

    let sub = args[0].to_ascii_lowercase();
    match sub.as_str() {
        "on" | "enable" => {
            if let Some(task) = args
                .get(1..)
                .map(|parts| parts.join(" "))
                .filter(|t| !t.trim().is_empty())
            {
                plan_mode_run_task(host, &task).await?;
            } else {
                host.agent().set_plan_phase(PlanPhase::Planning);
                emit_command_output(
                    host,
                    "Plan mode ON: agent will research with read-only tools, submit a plan, and wait for approval.",
                );
            }
        }
        "off" | "disable" => {
            host.agent().set_plan_phase(PlanPhase::Off);
            host.agent().set_pending_plan(None);
            emit_command_output(host, "Plan mode OFF.");
        }
        "status" | "show" => {
            let phase = host.agent().plan_phase();
            let pending = host
                .agent()
                .pending_plan()
                .map(|p| format!("\nPending plan ({} chars).", p.chars().count()))
                .unwrap_or_default();
            emit_command_output(
                host,
                format!("Plan mode phase: {}{}", phase.as_str(), pending),
            );
        }
        "approve" | "accept" | "a" => {
            if host.agent().plan_phase() != PlanPhase::AwaitingApproval {
                emit_command_output(
                    host,
                    "No plan awaiting approval. Use /plan-mode <task> or /plan-mode on first.",
                );
                return Ok(CommandResult::Handled);
            }
            host.agent().set_plan_phase(PlanPhase::Executing);
            host.messages_mut().push(Message::user(
                "Plan approved. Proceed with execution.".to_string(),
            ));
            host.run_agent_turn().await?;
        }
        "reject" | "deny" | "r" => {
            host.agent().set_plan_phase(PlanPhase::Planning);
            host.agent().set_pending_plan(None);
            let feedback = args.get(1..).map(|parts| parts.join(" ")).unwrap_or_default();
            if feedback.trim().is_empty() {
                emit_command_output(
                    host,
                    "Plan rejected. Revise your request or run /plan-mode on again.",
                );
            } else {
                host.messages_mut().push(Message::user(format!(
                    "Plan rejected. User feedback: {feedback}"
                )));
                host.run_agent_turn().await?;
            }
        }
        "edit" | "e" => {
            let text = args.get(1..).map(|parts| parts.join(" ")).unwrap_or_default();
            if text.trim().is_empty() {
                emit_command_output(host, "Usage: /plan-mode edit <revised plan text>");
                return Ok(CommandResult::Handled);
            }
            host.agent().set_pending_plan(Some(text.clone()));
            host.agent().set_plan_phase(PlanPhase::Executing);
            host.messages_mut().push(Message::user(format!(
                "Plan updated and approved:\n{text}"
            )));
            host.run_agent_turn().await?;
        }
        _ => {
            let task = args.join(" ");
            plan_mode_run_task(host, &task).await?;
        }
    }
    Ok(CommandResult::Handled)
}

#[cfg(test)]
mod plan_mode_command_tests {
    use super::is_plan_mode_subcommand;

    #[test]
    fn plan_mode_subcommand_detection() {
        assert!(is_plan_mode_subcommand("on"));
        assert!(is_plan_mode_subcommand("APPROVE"));
        assert!(!is_plan_mode_subcommand("帮我做推广"));
        assert!(!is_plan_mode_subcommand("make"));
    }
}
