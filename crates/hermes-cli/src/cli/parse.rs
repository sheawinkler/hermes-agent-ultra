//! Staged CLI parsing to keep debug stack usage bounded.

use std::ffi::OsString;

use super::commands;
use super::globals::GlobalCli;
use super::shallow::ShallowCommand;
use super::types::{Cli, CliCommand};

impl Cli {
    pub fn try_parse() -> Result<Self, clap::Error> {
        Self::try_parse_from(std::env::args_os())
    }

    pub fn parse() -> Self {
        Self::try_parse().unwrap_or_else(|e| e.exit())
    }

    pub fn parse_from<I, T>(itr: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::try_parse_from(itr).unwrap_or_else(|e| e.exit())
    }

    pub fn try_parse_from<I, T>(itr: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let (globals, args) = GlobalCli::parse_shallow(itr)?;
        let command = match globals.command {
            None => None,
            Some(ShallowCommand::Hermes) => Some(CliCommand::Hermes),
            Some(ShallowCommand::Setup { .. }) => Some(commands::parse_setup(&args)?),
            Some(ShallowCommand::Status) => Some(CliCommand::Status),
            Some(ShallowCommand::Version) => Some(CliCommand::Version),
            Some(ShallowCommand::PluginExternal(parts)) => Some(CliCommand::PluginExternal(parts)),
            Some(other) => Some(parse_shallow_command(&other, &args)?),
        };

        Ok(Cli {
            command,
            verbose: globals.flags.verbose,
            config_dir: globals.flags.config_dir,
            model: globals.flags.model,
            provider: globals.flags.provider,
            oneshot: globals.flags.oneshot,
            allow_tools: globals.flags.allow_tools,
            personality: globals.flags.personality,
            ignore_user_config: globals.flags.ignore_user_config,
            ignore_rules: globals.flags.ignore_rules,
            accept_hooks: globals.flags.accept_hooks,
        })
    }
}

fn parse_shallow_command(
    command: &ShallowCommand,
    args: &[OsString],
) -> Result<CliCommand, clap::Error> {
    match command {
        ShallowCommand::Model { .. } => commands::parse_model(args),
        ShallowCommand::Tools { .. } => commands::parse_tools(args),
        ShallowCommand::Config { .. } => commands::parse_config(args),
        ShallowCommand::Gateway { .. } => commands::parse_gateway(args),
        ShallowCommand::Doctor { .. } => commands::parse_doctor(args),
        ShallowCommand::Update { .. } => commands::parse_update(args),
        ShallowCommand::EliteCheck { .. } => commands::parse_elite_check(args),
        ShallowCommand::VerifyProvenance { .. } => commands::parse_verify_provenance(args),
        ShallowCommand::RotateProvenanceKey { .. } => commands::parse_rotate_provenance_key(args),
        ShallowCommand::RouteLearning { .. } => commands::parse_route_learning(args),
        ShallowCommand::RouteHealth { .. } => commands::parse_route_health(args),
        ShallowCommand::RouteAutotune { .. } => commands::parse_route_autotune(args),
        ShallowCommand::IncidentPack { .. } => commands::parse_incident_pack(args),
        ShallowCommand::Portal { .. } => commands::parse_portal(args),
        ShallowCommand::Systems { .. } => commands::parse_systems(args),
        ShallowCommand::Kanban { .. } => commands::parse_kanban(args),
        ShallowCommand::TeamsPipeline { .. } => commands::parse_teams_pipeline(args),
        ShallowCommand::Dashboard { .. } => commands::parse_dashboard(args),
        ShallowCommand::Debug { .. } => commands::parse_debug(args),
        ShallowCommand::Logs { .. } => commands::parse_logs(args),
        ShallowCommand::Profile { .. } => commands::parse_profile(args),
        ShallowCommand::Auth { .. } => commands::parse_auth(args),
        ShallowCommand::Secrets { .. } => commands::parse_secrets(args),
        ShallowCommand::Cron { .. } => commands::parse_cron(args),
        ShallowCommand::Webhook { .. } => commands::parse_webhook(args),
        ShallowCommand::Chat { .. } => commands::parse_chat(args),
        ShallowCommand::Skills { .. } => commands::parse_skills(args),
        ShallowCommand::Plugins { .. } => commands::parse_plugins(args),
        ShallowCommand::Memory { .. } => commands::parse_memory(args),
        ShallowCommand::Meeting { .. } => commands::parse_meeting(args),
        #[cfg(any(feature = "talk", feature = "talk-rockchip"))]
        ShallowCommand::Talk { .. } => commands::parse_talk(args),
        ShallowCommand::Interest { .. } => commands::parse_interest(args),
        ShallowCommand::Contribute { .. } => commands::parse_contribute(args),
        ShallowCommand::Server { .. } => commands::parse_server(args),
        ShallowCommand::Media { .. } => commands::parse_media(args),
        ShallowCommand::Mcp { .. } => commands::parse_mcp(args),
        ShallowCommand::Sessions { .. } => commands::parse_sessions(args),
        ShallowCommand::Resume { .. } => commands::parse_resume(args),
        ShallowCommand::Insights { .. } => commands::parse_insights(args),
        ShallowCommand::Login { .. } => commands::parse_login(args),
        ShallowCommand::Logout { .. } => commands::parse_logout(args),
        ShallowCommand::Whatsapp { .. } => commands::parse_whatsapp(args),
        ShallowCommand::Pairing { .. } => commands::parse_pairing(args),
        ShallowCommand::Claw { .. } => commands::parse_claw(args),
        ShallowCommand::Acp { .. } => commands::parse_acp(args),
        ShallowCommand::Backup { .. } => commands::parse_backup(args),
        ShallowCommand::Import { .. } => commands::parse_import(args),
        ShallowCommand::Dump { .. } => commands::parse_dump(args),
        ShallowCommand::Completion { .. } => commands::parse_completion(args),
        ShallowCommand::Uninstall { .. } => commands::parse_uninstall(args),
        ShallowCommand::Lumio { .. } => commands::parse_lumio(args),
        ShallowCommand::EnsureDep { .. } => commands::parse_ensure_dep(args),
        ShallowCommand::Hermes
        | ShallowCommand::Setup { .. }
        | ShallowCommand::Status
        | ShallowCommand::Version
        | ShallowCommand::PluginExternal(_) => unreachable!("handled in first pass"),
    }
}
