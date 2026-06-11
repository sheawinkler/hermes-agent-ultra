use hermes_core::AgentError;

use super::super::skills;
use crate::commands::{CommandResult, emit_command_output};

pub(crate) fn handle_ops_skills_tier_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            host,
            format!(
                "skills_tier={} (bypass={})",
                skills::skills_execution_tier().as_str(),
                if skills::skills_tier_bypass_enabled() {
                    "ON"
                } else {
                    "OFF"
                }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let Some(next) = skills::SkillsExecutionTier::parse(args[0]) else {
        emit_command_output(
            host,
            "Usage: /ops skills-tier [status|trusted|balanced|open]",
        );
        return Ok(CommandResult::Handled);
    };
    crate::env_vars::set_var("HERMES_SKILLS_EXECUTION_TIER", next.as_str());
    emit_command_output(
        host,
        format!(
            "skills_tier set to '{}' for this runtime process.",
            next.as_str()
        ),
    );
    Ok(CommandResult::Handled)
}
