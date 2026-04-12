//! CLI argument parsing using clap (Requirement 9.7).
//!
//! Defines the command-line interface for the hermes binary.

use clap::{Parser, Subcommand};

// ---------------------------------------------------------------------------
// CliCommand
// ---------------------------------------------------------------------------

/// Top-level subcommands for the hermes CLI.
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Start an interactive session (default when no subcommand is given).
    #[command(name = "hermes")]
    Hermes,

    /// Show or set the current model.
    ///
    /// Examples:
    ///   hermes model                    — show current model
    ///   hermes model openai:gpt-4o      — switch to gpt-4o via openai provider
    Model {
        /// Provider:model identifier (e.g. "openai:gpt-4o", "anthropic:claude-3-opus").
        provider_model: Option<String>,
    },

    /// List or manage available tools.
    ///
    /// Examples:
    ///   hermes tools                    — list all registered tools
    ///   hermes tools enable web_search  — enable a specific tool
    ///   hermes tools disable bash       — disable a specific tool
    Tools {
        /// Action: "list", "enable <name>", or "disable <name>".
        action: Option<String>,
    },

    /// Configuration management.
    ///
    /// Examples:
    ///   hermes config                   — show full configuration
    ///   hermes config get model         — get a specific config key
    ///   hermes config set model gpt-4o  — set a config key
    Config {
        /// Action: "get", "set", or omitted to show all.
        action: Option<String>,
        /// Configuration key (e.g. "model", "max_turns").
        key: Option<String>,
        /// Configuration value (used with "set" action).
        value: Option<String>,
    },

    /// Start or manage the gateway server.
    ///
    /// Examples:
    ///   hermes gateway start            — start the gateway
    ///   hermes gateway status           — check gateway status
    Gateway {
        /// Action: "start", "stop", "restart", or "status".
        action: Option<String>,
    },

    /// Run the interactive setup wizard.
    Setup,

    /// Check dependencies and configuration health.
    Doctor,

    /// Check for updates.
    Update,

    /// Show running status (active sessions, model, uptime).
    Status,

    /// Show recent logs.
    ///
    /// Examples:
    ///   hermes logs              — show last 20 log entries
    ///   hermes logs 50           — show last 50 log entries
    ///   hermes logs --follow     — tail logs in real-time
    Logs {
        /// Number of recent log entries to show (default: 20).
        #[arg(default_value = "20")]
        lines: u32,
        /// Tail the log file in real-time.
        #[arg(short, long)]
        follow: bool,
    },

    /// Profile management (list, switch, create).
    ///
    /// Examples:
    ///   hermes profile              — show current profile
    ///   hermes profile list         — list all profiles
    ///   hermes profile create work  — create a new profile named "work"
    ///   hermes profile switch work  — switch to the "work" profile
    Profile {
        /// Action: "list", "create <name>", "switch <name>", or omitted to show current.
        action: Option<String>,
        /// Profile name (used with "create" and "switch" actions).
        name: Option<String>,
    },

    /// Authentication management.
    Auth {
        /// Action: "login", "logout", "status".
        action: Option<String>,
        /// Provider: openai/anthropic/... / `telegram` (writes Bot token to config.yaml) / `copilot`.
        /// If omitted, uses `HERMES_AUTH_DEFAULT_PROVIDER` or `openai`.
        provider: Option<String>,
    },

    /// Cron management commands.
    Cron {
        /// Action: list/create/delete/pause/resume/run/history
        action: Option<String>,
        /// Job id (delete/pause/resume/run/history).
        #[arg(long)]
        id: Option<String>,
        /// Cron schedule (create), e.g. "0 9 * * *".
        #[arg(long)]
        schedule: Option<String>,
        /// Prompt text (create).
        #[arg(long)]
        prompt: Option<String>,
    },

    /// Webhook management commands (local registry in `webhooks.json`; `hermes gateway start` POSTs cron completion JSON to each URL).
    Webhook {
        /// Action: list/add/remove.
        action: Option<String>,
        /// Webhook URL (add, or remove by URL).
        #[arg(long)]
        url: Option<String>,
        /// Entry id (remove by id).
        #[arg(long)]
        id: Option<String>,
    },

    /// Start an interactive chat session.
    Chat {
        /// Single-shot query (non-interactive).
        #[arg(short, long)]
        query: Option<String>,
        /// Preload a skill before chatting.
        #[arg(long)]
        preload_skill: Option<String>,
        /// Skip confirmation for dangerous tools.
        #[arg(long)]
        yolo: bool,
    },

    /// Skills management.
    Skills {
        /// Action: browse/search/install/inspect/list/check/update/audit/uninstall/publish/snapshot/tap/config
        action: Option<String>,
        /// Skill name or search query.
        name: Option<String>,
        /// Additional argument (e.g. tap URL, snapshot path).
        #[arg(long)]
        extra: Option<String>,
    },

    /// Plugin management.
    Plugins {
        /// Action: install/update/remove/list/enable/disable
        action: Option<String>,
        /// Plugin name.
        name: Option<String>,
        /// Git branch, tag, or commit to checkout after clone (remote installs only).
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Allow clone from hosts outside the default allowlist (high risk).
        #[arg(long)]
        allow_untrusted_git_host: bool,
    },

    /// Memory provider management.
    Memory {
        /// Action: setup/status/off
        action: Option<String>,
    },

    /// MCP server management.
    Mcp {
        /// Action: serve/add/remove/list/test/configure
        action: Option<String>,
        /// Server name or URL.
        #[arg(long)]
        server: Option<String>,
    },

    /// Session management.
    Sessions {
        /// Action: list/export/delete/prune/stats/rename/browse
        action: Option<String>,
        /// Session ID.
        #[arg(long)]
        id: Option<String>,
        /// New name (for rename).
        #[arg(long)]
        name: Option<String>,
    },

    /// Usage analytics and insights.
    Insights {
        /// Number of days to analyze.
        #[arg(long, default_value = "30")]
        days: u32,
        /// Filter by source.
        #[arg(long)]
        source: Option<String>,
    },

    /// Login to a provider.
    Login {
        /// Provider name (openai/anthropic/nous/copilot).
        provider: Option<String>,
    },

    /// Logout from a provider.
    Logout {
        /// Provider name.
        provider: Option<String>,
    },

    /// WhatsApp-specific configuration.
    Whatsapp {
        /// Action: setup/status/qr
        action: Option<String>,
    },

    /// Device pairing management.
    Pairing {
        /// Action: list/approve/revoke/clear-pending
        action: Option<String>,
        /// Device ID.
        #[arg(long)]
        device_id: Option<String>,
    },

    /// OpenClaw migration utilities.
    Claw {
        /// Action: migrate/cleanup
        action: Option<String>,
    },

    /// ACP (Agent Communication Protocol) server.
    Acp {
        /// Action: start/status
        action: Option<String>,
    },

    /// Backup configuration and sessions.
    Backup {
        /// Output path for backup archive.
        output: Option<String>,
    },

    /// Import configuration from backup.
    Import {
        /// Path to backup archive.
        path: String,
    },

    /// Show version information.
    Version,

    /// Export conversation/session dump.
    Dump {
        /// Session id or file stem.
        session: Option<String>,
        /// Output path.
        output: Option<String>,
    },

    /// Generate shell completion scripts.
    Completion {
        /// Shell type: bash/zsh/fish/powershell/elvish.
        shell: Option<String>,
    },

    /// Uninstall helper (removes ~/.hermes by default).
    Uninstall {
        /// Confirm destructive cleanup.
        #[arg(long)]
        yes: bool,
    },
}

// ---------------------------------------------------------------------------
// Cli
// ---------------------------------------------------------------------------

/// Hermes Agent CLI.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "hermes",
    version,
    about = "Hermes Agent — autonomous AI agent with tool use",
    long_about = "Hermes Agent is an autonomous AI agent that can use tools, execute code, \
                  and interact with various platforms. Start an interactive session with `hermes` \
                  or use subcommands for specific tasks."
)]
pub struct Cli {
    /// The subcommand to execute. Defaults to starting an interactive session.
    #[command(subcommand)]
    pub command: Option<CliCommand>,

    /// Enable verbose / debug logging.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Override the configuration directory path.
    #[arg(short = 'C', long, global = true)]
    pub config_dir: Option<String>,

    /// Override the default model (e.g. "openai:gpt-4o").
    #[arg(short = 'm', long, global = true)]
    pub model: Option<String>,

    /// Override the personality / persona.
    #[arg(short = 'p', long, global = true)]
    pub personality: Option<String>,
}

impl Cli {
    /// Return the effective command, defaulting to `CliCommand::Hermes`.
    pub fn effective_command(&self) -> CliCommand {
        self.command.clone().unwrap_or(CliCommand::Hermes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_parse_default() {
        let cli = Cli::try_parse_from(vec!["hermes"]).unwrap();
        assert!(cli.command.is_none());
        assert!(!cli.verbose);
        assert!(cli.config_dir.is_none());
        assert!(cli.model.is_none());
    }

    #[test]
    fn cli_parse_model() {
        let cli = Cli::try_parse_from(vec!["hermes", "model", "openai:gpt-4o"]).unwrap();
        match cli.command {
            Some(CliCommand::Model { provider_model }) => {
                assert_eq!(provider_model.as_deref(), Some("openai:gpt-4o"));
            }
            _ => panic!("Expected Model command"),
        }
    }

    #[test]
    fn cli_parse_verbose() {
        let cli = Cli::try_parse_from(vec!["hermes", "-v"]).unwrap();
        assert!(cli.verbose);
    }

    #[test]
    fn cli_parse_config_dir() {
        let cli = Cli::try_parse_from(vec!["hermes", "-C", "/tmp/hermes"]).unwrap();
        assert_eq!(cli.config_dir.as_deref(), Some("/tmp/hermes"));
    }

    #[test]
    fn cli_parse_model_flag() {
        let cli = Cli::try_parse_from(vec!["hermes", "-m", "claude-3-opus"]).unwrap();
        assert_eq!(cli.model.as_deref(), Some("claude-3-opus"));
    }

    #[test]
    fn cli_effective_command_default() {
        let cli = Cli::try_parse_from(vec!["hermes"]).unwrap();
        assert!(matches!(cli.effective_command(), CliCommand::Hermes));
    }

    #[test]
    fn cli_parse_doctor() {
        let cli = Cli::try_parse_from(vec!["hermes", "doctor"]).unwrap();
        assert!(matches!(cli.command, Some(CliCommand::Doctor)));
    }

    #[test]
    fn cli_parse_status() {
        let cli = Cli::try_parse_from(vec!["hermes", "status"]).unwrap();
        assert!(matches!(cli.command, Some(CliCommand::Status)));
    }

    #[test]
    fn cli_parse_logs_default() {
        let cli = Cli::try_parse_from(vec!["hermes", "logs"]).unwrap();
        match cli.command {
            Some(CliCommand::Logs { lines, follow }) => {
                assert_eq!(lines, 20);
                assert!(!follow);
            }
            _ => panic!("Expected Logs command"),
        }
    }

    #[test]
    fn cli_parse_logs_with_count() {
        let cli = Cli::try_parse_from(vec!["hermes", "logs", "50"]).unwrap();
        match cli.command {
            Some(CliCommand::Logs { lines, .. }) => {
                assert_eq!(lines, 50);
            }
            _ => panic!("Expected Logs command"),
        }
    }

    #[test]
    fn cli_parse_profile() {
        let cli = Cli::try_parse_from(vec!["hermes", "profile", "list"]).unwrap();
        match cli.command {
            Some(CliCommand::Profile { action, .. }) => {
                assert_eq!(action.as_deref(), Some("list"));
            }
            _ => panic!("Expected Profile command"),
        }
    }

    #[test]
    fn cli_parse_profile_create() {
        let cli = Cli::try_parse_from(vec!["hermes", "profile", "create", "work"]).unwrap();
        match cli.command {
            Some(CliCommand::Profile { action, name }) => {
                assert_eq!(action.as_deref(), Some("create"));
                assert_eq!(name.as_deref(), Some("work"));
            }
            _ => panic!("Expected Profile command"),
        }
    }

    #[test]
    fn cli_parse_config_set() {
        let cli = Cli::try_parse_from(vec!["hermes", "config", "set", "model", "gpt-4o"]).unwrap();
        match cli.command {
            Some(CliCommand::Config { action, key, value }) => {
                assert_eq!(action.as_deref(), Some("set"));
                assert_eq!(key.as_deref(), Some("model"));
                assert_eq!(value.as_deref(), Some("gpt-4o"));
            }
            _ => panic!("Expected Config command"),
        }
    }
}
