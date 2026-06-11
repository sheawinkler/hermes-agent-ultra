use hermes_core::AgentError;

use crate::commands::{CommandResult, emit_command_output};

pub(crate) fn handle_ops_tool_profile_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let mode = std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());
    if args.is_empty()
        || args
            .first()
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "status" | "show"))
    {
        emit_command_output(
            host,
            format!(
                "repo_review_tool_profile mode={}\nUse `/ops tool-profile [off|balanced|focus]`.\nEscape hatch: include `allow all tools` or `disable narrowing` in your request.",
                mode
            ),
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("list") {
        emit_command_output(
            host,
            "Repo-review tool profile modes:\n- off: disable narrowing (open tool lane)\n- balanced: filter messaging/non-repo noise only\n- focus: balanced + stricter repo-first filtering",
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("clear") {
        crate::env_vars::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
        emit_command_output(
            host,
            "Cleared repo-review tool profile override (default=balanced).",
        );
        return Ok(CommandResult::Handled);
    }
    let next = args[0].to_ascii_lowercase();
    if !matches!(next.as_str(), "off" | "balanced" | "focus") {
        emit_command_output(
            host,
            "Usage: /ops tool-profile [status|list|off|balanced|focus|clear]",
        );
        return Ok(CommandResult::Handled);
    }
    crate::env_vars::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", next.as_str());
    emit_command_output(
        host,
        format!("repo_review_tool_profile mode set to `{}`", next),
    );
    Ok(CommandResult::Handled)
}
