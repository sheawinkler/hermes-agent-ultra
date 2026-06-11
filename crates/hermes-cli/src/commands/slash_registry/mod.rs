mod invoke;

use std::collections::HashMap;
use std::sync::LazyLock;

use hermes_core::AgentError;

use super::autocomplete::{canonical_command, expand_quick_alias_command};
use super::{CommandResult, emit_command_output};
use invoke::invoke_handler;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SlashHandlerId {
    New,
    Retry,
    Undo,
    History,
    Recap,
    Context,
    Title,
    Branch,
    Timetravel,
    Snapshot,
    Rollback,
    Queue,
    Handoff,
    Steer,
    Btw,
    Subgoal,
    Sethome,
    Evolve,
    Objective,
    Claims,
    Quorum,
    Swarm,
    Simulate,
    Specpatch,
    Heatmap,
    Studio,
    Ask,
    Model,
    Auth,
    Provider,
    Personality,
    Profile,
    Fast,
    Skin,
    Voice,
    Pet,
    Skills,
    Curator,
    Tools,
    Toolcards,
    Toolsets,
    Plugins,
    Mcp,
    Reload,
    ReloadMcp,
    Cron,
    Agents,
    Kanban,
    Plan,
    PlanMode,
    Lsp,
    Graph,
    Qos,
    Image,
    Config,
    Autocompact,
    Compress,
    ClearQueue,
    Usage,
    Insights,
    Stop,
    Status,
    About,
    Ops,
    Telemetry,
    Runbook,
    Eval,
    Autopilot,
    Mission,
    Dashboard,
    Platforms,
    Integrations,
    Commands,
    Boot,
    Walkthrough,
    Triage,
    Subconscious,
    Log,
    DebugDump,
    DumpFormat,
    Experiment,
    Feedback,
    Restart,
    Update,
    Redraw,
    Paste,
    Gquota,
    Approve,
    Deny,
    Copy,
    Save,
    Load,
    Resume,
    Sessions,
    Background,
    Mouse,
    Verbose,
    Statusbar,
    Yolo,
    Browser,
    Reasoning,
    Raw,
    Policy,
    Help,
    AcpServer,
    Quit,
}

struct RegistryEntry {
    id: SlashHandlerId,
    canonical: &'static str,
}

static REGISTRY_ENTRIES: &[RegistryEntry] = &[
    RegistryEntry {
        id: SlashHandlerId::New,
        canonical: "/new",
    },
    RegistryEntry {
        id: SlashHandlerId::Retry,
        canonical: "/retry",
    },
    RegistryEntry {
        id: SlashHandlerId::Undo,
        canonical: "/undo",
    },
    RegistryEntry {
        id: SlashHandlerId::History,
        canonical: "/history",
    },
    RegistryEntry {
        id: SlashHandlerId::Recap,
        canonical: "/recap",
    },
    RegistryEntry {
        id: SlashHandlerId::Context,
        canonical: "/context",
    },
    RegistryEntry {
        id: SlashHandlerId::Title,
        canonical: "/title",
    },
    RegistryEntry {
        id: SlashHandlerId::Branch,
        canonical: "/branch",
    },
    RegistryEntry {
        id: SlashHandlerId::Timetravel,
        canonical: "/timetravel",
    },
    RegistryEntry {
        id: SlashHandlerId::Snapshot,
        canonical: "/snapshot",
    },
    RegistryEntry {
        id: SlashHandlerId::Rollback,
        canonical: "/rollback",
    },
    RegistryEntry {
        id: SlashHandlerId::Queue,
        canonical: "/queue",
    },
    RegistryEntry {
        id: SlashHandlerId::Handoff,
        canonical: "/handoff",
    },
    RegistryEntry {
        id: SlashHandlerId::Steer,
        canonical: "/steer",
    },
    RegistryEntry {
        id: SlashHandlerId::Btw,
        canonical: "/btw",
    },
    RegistryEntry {
        id: SlashHandlerId::Subgoal,
        canonical: "/subgoal",
    },
    RegistryEntry {
        id: SlashHandlerId::Sethome,
        canonical: "/sethome",
    },
    RegistryEntry {
        id: SlashHandlerId::Evolve,
        canonical: "/evolve",
    },
    RegistryEntry {
        id: SlashHandlerId::Objective,
        canonical: "/objective",
    },
    RegistryEntry {
        id: SlashHandlerId::Claims,
        canonical: "/claims",
    },
    RegistryEntry {
        id: SlashHandlerId::Quorum,
        canonical: "/quorum",
    },
    RegistryEntry {
        id: SlashHandlerId::Swarm,
        canonical: "/swarm",
    },
    RegistryEntry {
        id: SlashHandlerId::Simulate,
        canonical: "/simulate",
    },
    RegistryEntry {
        id: SlashHandlerId::Specpatch,
        canonical: "/specpatch",
    },
    RegistryEntry {
        id: SlashHandlerId::Heatmap,
        canonical: "/heatmap",
    },
    RegistryEntry {
        id: SlashHandlerId::Studio,
        canonical: "/studio",
    },
    RegistryEntry {
        id: SlashHandlerId::Ask,
        canonical: "/ask",
    },
    RegistryEntry {
        id: SlashHandlerId::Model,
        canonical: "/model",
    },
    RegistryEntry {
        id: SlashHandlerId::Auth,
        canonical: "/auth",
    },
    RegistryEntry {
        id: SlashHandlerId::Provider,
        canonical: "/provider",
    },
    RegistryEntry {
        id: SlashHandlerId::Personality,
        canonical: "/personality",
    },
    RegistryEntry {
        id: SlashHandlerId::Profile,
        canonical: "/profile",
    },
    RegistryEntry {
        id: SlashHandlerId::Fast,
        canonical: "/fast",
    },
    RegistryEntry {
        id: SlashHandlerId::Skin,
        canonical: "/skin",
    },
    RegistryEntry {
        id: SlashHandlerId::Voice,
        canonical: "/voice",
    },
    RegistryEntry {
        id: SlashHandlerId::Pet,
        canonical: "/pet",
    },
    RegistryEntry {
        id: SlashHandlerId::Skills,
        canonical: "/skills",
    },
    RegistryEntry {
        id: SlashHandlerId::Curator,
        canonical: "/curator",
    },
    RegistryEntry {
        id: SlashHandlerId::Tools,
        canonical: "/tools",
    },
    RegistryEntry {
        id: SlashHandlerId::Toolcards,
        canonical: "/toolcards",
    },
    RegistryEntry {
        id: SlashHandlerId::Toolsets,
        canonical: "/toolsets",
    },
    RegistryEntry {
        id: SlashHandlerId::Plugins,
        canonical: "/plugins",
    },
    RegistryEntry {
        id: SlashHandlerId::Mcp,
        canonical: "/mcp",
    },
    RegistryEntry {
        id: SlashHandlerId::Reload,
        canonical: "/reload",
    },
    RegistryEntry {
        id: SlashHandlerId::ReloadMcp,
        canonical: "/reload-mcp",
    },
    RegistryEntry {
        id: SlashHandlerId::Cron,
        canonical: "/cron",
    },
    RegistryEntry {
        id: SlashHandlerId::Agents,
        canonical: "/agents",
    },
    RegistryEntry {
        id: SlashHandlerId::Kanban,
        canonical: "/kanban",
    },
    RegistryEntry {
        id: SlashHandlerId::Plan,
        canonical: "/plan",
    },
    RegistryEntry {
        id: SlashHandlerId::PlanMode,
        canonical: "/plan-mode",
    },
    RegistryEntry {
        id: SlashHandlerId::Lsp,
        canonical: "/lsp",
    },
    RegistryEntry {
        id: SlashHandlerId::Graph,
        canonical: "/graph",
    },
    RegistryEntry {
        id: SlashHandlerId::Qos,
        canonical: "/qos",
    },
    RegistryEntry {
        id: SlashHandlerId::Image,
        canonical: "/image",
    },
    RegistryEntry {
        id: SlashHandlerId::Config,
        canonical: "/config",
    },
    RegistryEntry {
        id: SlashHandlerId::Autocompact,
        canonical: "/autocompact",
    },
    RegistryEntry {
        id: SlashHandlerId::Compress,
        canonical: "/compress",
    },
    RegistryEntry {
        id: SlashHandlerId::ClearQueue,
        canonical: "/clear-queue",
    },
    RegistryEntry {
        id: SlashHandlerId::Usage,
        canonical: "/usage",
    },
    RegistryEntry {
        id: SlashHandlerId::Insights,
        canonical: "/insights",
    },
    RegistryEntry {
        id: SlashHandlerId::Stop,
        canonical: "/stop",
    },
    RegistryEntry {
        id: SlashHandlerId::Status,
        canonical: "/status",
    },
    RegistryEntry {
        id: SlashHandlerId::About,
        canonical: "/about",
    },
    RegistryEntry {
        id: SlashHandlerId::Ops,
        canonical: "/ops",
    },
    RegistryEntry {
        id: SlashHandlerId::Telemetry,
        canonical: "/telemetry",
    },
    RegistryEntry {
        id: SlashHandlerId::Runbook,
        canonical: "/runbook",
    },
    RegistryEntry {
        id: SlashHandlerId::Eval,
        canonical: "/eval",
    },
    RegistryEntry {
        id: SlashHandlerId::Autopilot,
        canonical: "/autopilot",
    },
    RegistryEntry {
        id: SlashHandlerId::Mission,
        canonical: "/mission",
    },
    RegistryEntry {
        id: SlashHandlerId::Dashboard,
        canonical: "/dashboard",
    },
    RegistryEntry {
        id: SlashHandlerId::Platforms,
        canonical: "/platforms",
    },
    RegistryEntry {
        id: SlashHandlerId::Integrations,
        canonical: "/integrations",
    },
    RegistryEntry {
        id: SlashHandlerId::Commands,
        canonical: "/commands",
    },
    RegistryEntry {
        id: SlashHandlerId::Boot,
        canonical: "/boot",
    },
    RegistryEntry {
        id: SlashHandlerId::Walkthrough,
        canonical: "/walkthrough",
    },
    RegistryEntry {
        id: SlashHandlerId::Triage,
        canonical: "/triage",
    },
    RegistryEntry {
        id: SlashHandlerId::Subconscious,
        canonical: "/subconscious",
    },
    RegistryEntry {
        id: SlashHandlerId::Log,
        canonical: "/log",
    },
    RegistryEntry {
        id: SlashHandlerId::DebugDump,
        canonical: "/debug-dump",
    },
    RegistryEntry {
        id: SlashHandlerId::DumpFormat,
        canonical: "/dump-format",
    },
    RegistryEntry {
        id: SlashHandlerId::Experiment,
        canonical: "/experiment",
    },
    RegistryEntry {
        id: SlashHandlerId::Feedback,
        canonical: "/feedback",
    },
    RegistryEntry {
        id: SlashHandlerId::Restart,
        canonical: "/restart",
    },
    RegistryEntry {
        id: SlashHandlerId::Update,
        canonical: "/update",
    },
    RegistryEntry {
        id: SlashHandlerId::Redraw,
        canonical: "/redraw",
    },
    RegistryEntry {
        id: SlashHandlerId::Paste,
        canonical: "/paste",
    },
    RegistryEntry {
        id: SlashHandlerId::Gquota,
        canonical: "/gquota",
    },
    RegistryEntry {
        id: SlashHandlerId::Approve,
        canonical: "/approve",
    },
    RegistryEntry {
        id: SlashHandlerId::Deny,
        canonical: "/deny",
    },
    RegistryEntry {
        id: SlashHandlerId::Copy,
        canonical: "/copy",
    },
    RegistryEntry {
        id: SlashHandlerId::Save,
        canonical: "/save",
    },
    RegistryEntry {
        id: SlashHandlerId::Load,
        canonical: "/load",
    },
    RegistryEntry {
        id: SlashHandlerId::Resume,
        canonical: "/resume",
    },
    RegistryEntry {
        id: SlashHandlerId::Sessions,
        canonical: "/sessions",
    },
    RegistryEntry {
        id: SlashHandlerId::Background,
        canonical: "/background",
    },
    RegistryEntry {
        id: SlashHandlerId::Mouse,
        canonical: "/mouse",
    },
    RegistryEntry {
        id: SlashHandlerId::Verbose,
        canonical: "/verbose",
    },
    RegistryEntry {
        id: SlashHandlerId::Statusbar,
        canonical: "/statusbar",
    },
    RegistryEntry {
        id: SlashHandlerId::Yolo,
        canonical: "/yolo",
    },
    RegistryEntry {
        id: SlashHandlerId::Browser,
        canonical: "/browser",
    },
    RegistryEntry {
        id: SlashHandlerId::Reasoning,
        canonical: "/reasoning",
    },
    RegistryEntry {
        id: SlashHandlerId::Raw,
        canonical: "/raw",
    },
    RegistryEntry {
        id: SlashHandlerId::Policy,
        canonical: "/policy",
    },
    RegistryEntry {
        id: SlashHandlerId::Help,
        canonical: "/help",
    },
    RegistryEntry {
        id: SlashHandlerId::AcpServer,
        canonical: "/acp_server",
    },
    RegistryEntry {
        id: SlashHandlerId::Quit,
        canonical: "/quit",
    },
];

static COMMAND_LOOKUP: LazyLock<HashMap<&'static str, SlashHandlerId>> = LazyLock::new(|| {
    REGISTRY_ENTRIES
        .iter()
        .map(|entry| (entry.canonical, entry.id))
        .collect()
});

pub(crate) async fn dispatch(
    host: &mut (impl crate::app::SlashCommandHost + crate::app::AcpServerRuntime),
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let canonical = canonical_command(cmd);
    match COMMAND_LOOKUP.get(canonical) {
        Some(id) => invoke_handler(*id, host, cmd, args).await,
        None => {
            emit_command_output(
                host,
                format!(
                    "Unknown command: {}. Type /help for available commands.",
                    cmd
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}

/// Handle a slash command.
///
/// `cmd` is the full command token including the `/` prefix
/// (e.g. `/model`, `/new`). `args` are the remaining tokens.
pub async fn handle_slash_command(
    host: &mut (impl crate::app::SlashCommandHost + crate::app::AcpServerRuntime),
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let (resolved_cmd, arg_storage) =
        match expand_quick_alias_command(&host.config().quick_commands, cmd, args) {
            Ok(expanded) => expanded,
            Err(message) => {
                emit_command_output(host, message);
                return Ok(CommandResult::Handled);
            }
        };
    let arg_refs: Vec<&str> = arg_storage.iter().map(|part| part.as_str()).collect();
    let args = arg_refs.as_slice();
    dispatch(host, resolved_cmd.as_str(), args).await
}
