//! Lightweight first-pass subcommand names (no per-command fields).

use clap::Subcommand;

/// First-pass subcommand routing table. Fields are parsed in a second pass.
#[derive(Debug, Clone, Subcommand)]
pub enum ShallowCommand {
    #[command(name = "hermes")]
    Hermes,
    #[command(disable_help_flag = true)]
    Model {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Tools {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Config {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Gateway {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Doctor {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Update {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "elite-check", disable_help_flag = true)]
    EliteCheck {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "verify-provenance", disable_help_flag = true)]
    VerifyProvenance {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "rotate-provenance-key", disable_help_flag = true)]
    RotateProvenanceKey {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "route-learning", disable_help_flag = true)]
    RouteLearning {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "route-health", disable_help_flag = true)]
    RouteHealth {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "route-autotune", disable_help_flag = true)]
    RouteAutotune {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "incident-pack", disable_help_flag = true)]
    IncidentPack {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Dashboard {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Debug {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Logs {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Profile {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Auth {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Secrets {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Cron {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Webhook {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Chat {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Skills {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Plugins {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Memory {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Meeting {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[cfg(any(feature = "talk", feature = "talk-rockchip"))]
    #[command(disable_help_flag = true)]
    Talk {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Interest {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Contribute {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Server {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Media {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Mcp {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Sessions {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Resume {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Insights {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Login {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Logout {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Whatsapp {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Pairing {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Claw {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Acp {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Backup {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Import {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Dump {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Completion {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Uninstall {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Lumio {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Setup {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Portal {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Systems {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(disable_help_flag = true)]
    Kanban {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "teams-pipeline", disable_help_flag = true)]
    TeamsPipeline {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    #[command(name = "_ensure-dep", hide = true, disable_help_flag = true)]
    EnsureDep {
        #[arg(trailing_var_arg = true, hide = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    Status,
    Version,
    #[command(external_subcommand)]
    PluginExternal(Vec<String>),
}
