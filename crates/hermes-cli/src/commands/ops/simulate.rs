use hermes_core::AgentError;

use crate::commands::{CommandResult, emit_command_output};

pub(crate) fn handle_simulate_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let counters = host.tool_registry().policy_counters();
        emit_command_output(
            host,
            format!(
                "Tool-policy simulation\n\
                 usage: /simulate <tool_name> [json-params]\n\
                 examples:\n  /simulate terminal {{\"cmd\":\"ls\"}}\n  /simulate skill_manage {{\"action\":\"view\",\"skill\":\"contextlattice-agent-contract\"}}\n\
                 counters: allow={} deny={} audit_only={} simulate={} would_block={}",
                counters.allow,
                counters.deny,
                counters.audit_only,
                counters.simulate,
                counters.would_block
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let tool_name = args[0].trim();
    if tool_name.is_empty() {
        emit_command_output(host, "Usage: /simulate <tool_name> [json-params]");
        return Ok(CommandResult::Handled);
    }
    let params = if args.len() > 1 {
        let raw = args[1..].join(" ");
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                emit_command_output(host, "simulate params must be a JSON object.");
                return Ok(CommandResult::Handled);
            }
            Err(err) => {
                emit_command_output(
                    host,
                    format!("simulate params parse error: {}\nraw={}", err, raw),
                );
                return Ok(CommandResult::Handled);
            }
        }
    } else {
        serde_json::json!({})
    };

    let decision = host
        .tool_registry()
        .evaluate_policy_preview(tool_name, &params);
    let payload = serde_json::json!({
        "tool": tool_name,
        "params": params,
        "decision": {
            "allow": decision.allow,
            "mode": decision.mode.as_str(),
            "audited_only": decision.audited_only,
            "simulated": decision.simulated,
            "would_block": decision.would_block,
            "code": decision.code,
            "reason": decision.reason,
        }
    });
    emit_command_output(
        host,
        serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Config(format!("serialize simulate result: {e}")))?,
    );
    Ok(CommandResult::Handled)
}
