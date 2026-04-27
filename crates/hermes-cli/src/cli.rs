//! CLI argument parsing using clap (Requirement 9.7).
//!
//! Defines the command-line interface for the hermes binaries.

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
        /// Tool name used by enable/disable actions.
        name: Option<String>,
        /// Optional platform scope for tool toggles.
        #[arg(long)]
        platform: Option<String>,
        /// Print compact per-platform summary (upstream parity alias).
        #[arg(long)]
        summary: bool,
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
        /// Action: "run", "start", "stop", "restart", "status", "install", "uninstall", "setup", or "migrate-legacy".
        action: Option<String>,
        /// Target system-level service scope when supported.
        #[arg(long)]
        system: bool,
        /// Apply action across all known gateway processes.
        #[arg(long)]
        all: bool,
        /// Force reinstall/overwrite for install action.
        #[arg(long)]
        force: bool,
        /// Linux system service run-as user.
        #[arg(long)]
        run_as_user: Option<String>,
        /// Replace existing foreground process when running.
        #[arg(long)]
        replace: bool,
        /// Dry-run for migration/install checks.
        #[arg(long)]
        dry_run: bool,
        /// Skip confirmation prompts.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Deep status checks.
        #[arg(long)]
        deep: bool,
    },

    /// Run the interactive setup wizard.
    Setup,

    /// Check dependencies and configuration health.
    Doctor {
        /// Run deeper diagnostics (gateway/runtime/memory endpoints).
        #[arg(long)]
        deep: bool,
        /// Perform safe local remediations (state dirs, stale pid, token perms).
        #[arg(long)]
        self_heal: bool,
        /// Write a machine-readable doctor snapshot JSON.
        #[arg(long)]
        snapshot: bool,
        /// Override snapshot output path.
        #[arg(long)]
        snapshot_path: Option<String>,
        /// Build a support bundle tar.gz with diagnostics artifacts.
        #[arg(long)]
        bundle: bool,
    },

    /// Check for updates.
    Update,

    /// Run consolidated elite diagnostics and release gates.
    EliteCheck {
        /// Print machine-readable JSON only.
        #[arg(long)]
        json: bool,
        /// Return non-zero on any failing elite gate.
        #[arg(long)]
        strict: bool,
    },

    /// Verify signed provenance sidecar for an artifact.
    VerifyProvenance {
        /// Artifact path to verify (e.g. doctor snapshot or replay manifest).
        path: String,
        /// Optional signature sidecar path override.
        #[arg(long)]
        signature: Option<String>,
        /// Enforce non-zero failure for any verification violation.
        #[arg(long)]
        strict: bool,
        /// Print compact machine-readable JSON only.
        #[arg(long)]
        json: bool,
    },

    /// Rotate the local provenance signing key.
    RotateProvenanceKey {
        /// Print compact machine-readable JSON only.
        #[arg(long)]
        json: bool,
    },

    /// Inspect or reset smart-routing learning state.
    RouteLearning {
        /// Action: show/list/reset/clear
        action: Option<String>,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Show running status (active sessions, model, uptime).
    Status,

    /// Start the dashboard-compatible local HTTP UI/API helper.
    Dashboard {
        /// Host bind address (default: 127.0.0.1).
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port (default: 9119).
        #[arg(long, default_value_t = 9119)]
        port: u16,
        /// Do not auto-open browser.
        #[arg(long)]
        no_open: bool,
        /// Allow binding to non-localhost.
        #[arg(long)]
        insecure: bool,
    },

    /// Debug diagnostics and support report helpers.
    Debug {
        /// Action: share/delete.
        action: Option<String>,
        /// Optional URL argument for delete action.
        url: Option<String>,
        /// Number of log lines for debug share.
        #[arg(long, default_value_t = 200)]
        lines: u32,
        /// Expiry days for uploaded report.
        #[arg(long, default_value_t = 7)]
        expire: u32,
        /// Print report locally instead of uploading.
        #[arg(long)]
        local: bool,
    },

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
        /// Action: "list", "use", "create", "delete", "show", "alias", "rename", "export", "import", or "switch" (legacy).
        action: Option<String>,
        /// Primary profile name or archive path depending on action.
        name: Option<String>,
        /// Secondary positional argument (e.g. new name for `rename`).
        secondary: Option<String>,
        /// Output path for `export`.
        #[arg(short, long)]
        output: Option<String>,
        /// Import profile name override for `import`.
        #[arg(long)]
        import_name: Option<String>,
        /// Optional wrapper alias name for `alias`.
        #[arg(long = "name")]
        alias_name: Option<String>,
        /// Remove wrapper alias for `alias`.
        #[arg(long)]
        remove: bool,
        /// Skip confirmation prompts.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Clone config essentials from active/source profile on create.
        #[arg(long)]
        clone: bool,
        /// Clone full profile state on create.
        #[arg(long = "clone-all")]
        clone_all: bool,
        /// Source profile when cloning.
        #[arg(long = "clone-from")]
        clone_from: Option<String>,
        /// Skip alias creation during profile create.
        #[arg(long = "no-alias")]
        no_alias: bool,
    },

    /// Authentication management.
    Auth {
        /// Action: "login", "logout", "status", "add", "list", "remove", or "reset".
        action: Option<String>,
        /// Provider: openai/anthropic/... / `telegram` / `weixin|wechat|wx` (write platform token to config.yaml) / `copilot`.
        /// If omitted, uses `HERMES_AUTH_DEFAULT_PROVIDER`, config model provider, or `nous`.
        provider: Option<String>,
        /// Action target (e.g. pooled credential index/id/label for `remove`).
        target: Option<String>,
        /// Credential type for `auth add` (oauth/api-key).
        #[arg(long = "type")]
        auth_type: Option<String>,
        /// Optional pooled credential label for `auth add`.
        #[arg(long)]
        label: Option<String>,
        /// API key value for `auth add` (otherwise interactive prompt).
        #[arg(long = "api-key")]
        api_key: Option<String>,
        /// For Weixin/QQBot login: prefer QR flow (scan-to-auth onboard).
        #[arg(long)]
        qr: bool,
    },

    /// Encrypted secret vault management.
    Secrets {
        /// Action: list/status/get/set/remove
        action: Option<String>,
        /// Provider key namespace (e.g. openai, anthropic, nous).
        provider: Option<String>,
        /// Secret value (used with `set`; if omitted, prompt is shown).
        #[arg(long)]
        value: Option<String>,
        /// Show full secret value for `get` (default masks output).
        #[arg(long)]
        show: bool,
    },

    /// Cron management commands.
    Cron {
        /// Action: list/create/edit/pause/resume/run/remove/delete/history/status/tick
        action: Option<String>,
        /// Job id (edit/delete/pause/resume/run/history/remove).
        job_id: Option<String>,
        /// Job id (legacy flag alias).
        #[arg(long)]
        id: Option<String>,
        /// Cron schedule (create), e.g. "0 9 * * *".
        #[arg(long)]
        schedule: Option<String>,
        /// Prompt text (create).
        #[arg(long)]
        prompt: Option<String>,
        /// Optional human-friendly job name.
        #[arg(long)]
        name: Option<String>,
        /// Delivery target override.
        #[arg(long)]
        deliver: Option<String>,
        /// Repeat count override.
        #[arg(long)]
        repeat: Option<u32>,
        /// Replace skills list.
        #[arg(long = "skill")]
        skills: Vec<String>,
        /// Append skills list.
        #[arg(long = "add-skill")]
        add_skills: Vec<String>,
        /// Remove skills from list.
        #[arg(long = "remove-skill")]
        remove_skills: Vec<String>,
        /// Clear all attached skills.
        #[arg(long)]
        clear_skills: bool,
        /// Script path/content field.
        #[arg(long)]
        script: Option<String>,
        /// Include inactive jobs (list action).
        #[arg(long)]
        all: bool,
    },

    /// Webhook management commands.
    Webhook {
        /// Action: subscribe/add/list/remove/rm/test.
        action: Option<String>,
        /// Route/subscription name.
        name: Option<String>,
        /// Webhook URL (add, or remove by URL).
        #[arg(long)]
        url: Option<String>,
        /// Entry id (remove by id).
        #[arg(long)]
        id: Option<String>,
        /// Prompt template for subscription routes.
        #[arg(long)]
        prompt: Option<String>,
        /// Comma-separated accepted events.
        #[arg(long)]
        events: Option<String>,
        /// Human-readable description.
        #[arg(long)]
        description: Option<String>,
        /// Comma-separated skills list.
        #[arg(long)]
        skills: Option<String>,
        /// Delivery target.
        #[arg(long)]
        deliver: Option<String>,
        /// Delivery chat id.
        #[arg(long = "deliver-chat-id")]
        deliver_chat_id: Option<String>,
        /// HMAC secret.
        #[arg(long)]
        secret: Option<String>,
        /// Skip agent execution and deliver prompt directly.
        #[arg(long = "deliver-only")]
        deliver_only: bool,
        /// Test payload JSON.
        #[arg(long)]
        payload: Option<String>,
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
        /// Action: browse/search/install/inspect/list/check/update/audit/uninstall/publish/snapshot/tap/config/reset/subscribe
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
        /// Action: setup/status/off/reset
        action: Option<String>,
        /// Reset target (all|memory|user) for `memory reset`.
        target: Option<String>,
        /// Skip reset confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// MCP server management.
    Mcp {
        /// Action: serve/add/remove/list/test/configure/login
        action: Option<String>,
        /// Server name.
        name: Option<String>,
        /// Legacy server name or URL option.
        #[arg(long)]
        server: Option<String>,
        /// URL for add action.
        #[arg(long)]
        url: Option<String>,
        /// Command for stdio server add action.
        #[arg(long)]
        command: Option<String>,
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
        /// Provider name (openai/anthropic/nous/copilot/telegram/weixin).
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
        /// Action: start/status/stop/restart
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

    /// Lumio API Gateway login and setup.
    ///
    /// Examples:
    ///   hermes lumio                    — login to Lumio via OAuth
    ///   hermes lumio --model gpt-4o     — login and set model
    ///   hermes lumio logout             — remove saved Lumio token
    ///   hermes lumio status             — show current Lumio login status
    Lumio {
        /// Action: login (default), logout, status.
        action: Option<String>,
        /// Model to use after login (default: deepseek/deepseek-chat).
        #[arg(short, long)]
        model: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Cli
// ---------------------------------------------------------------------------

/// Hermes Agent CLI.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "hermes-agent-ultra",
    version,
    about = "Hermes Agent Ultra — autonomous AI agent with tool use",
    long_about = "Hermes Agent Ultra is an autonomous AI agent that can use tools, execute code, \
                  and interact with various platforms. Start an interactive session with \
                  `hermes-agent-ultra` (or legacy alias `hermes`) or use subcommands for \
                  specific tasks."
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

    /// Override the provider for this invocation (e.g. "nous", "anthropic").
    #[arg(long, global = true)]
    pub provider: Option<String>,

    /// One-shot prompt mode (non-interactive), aliasing upstream `-z`.
    #[arg(short = 'z', long, global = true)]
    pub oneshot: Option<String>,

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
        assert!(cli.provider.is_none());
        assert!(cli.oneshot.is_none());
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
    fn cli_parse_provider_and_oneshot_flags() {
        let cli = Cli::try_parse_from(vec![
            "hermes",
            "--provider",
            "anthropic",
            "-z",
            "reply with 1",
        ])
        .unwrap();
        assert_eq!(cli.provider.as_deref(), Some("anthropic"));
        assert_eq!(cli.oneshot.as_deref(), Some("reply with 1"));
    }

    #[test]
    fn cli_effective_command_default() {
        let cli = Cli::try_parse_from(vec!["hermes"]).unwrap();
        assert!(matches!(cli.effective_command(), CliCommand::Hermes));
    }

    #[test]
    fn cli_parse_doctor() {
        let cli = Cli::try_parse_from(vec!["hermes", "doctor"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Doctor {
                deep: false,
                self_heal: false,
                snapshot: false,
                snapshot_path: None,
                bundle: false
            })
        ));
    }

    #[test]
    fn cli_parse_doctor_with_flags() {
        let cli = Cli::try_parse_from(vec![
            "hermes",
            "doctor",
            "--deep",
            "--snapshot",
            "--snapshot-path",
            "/tmp/doctor.json",
            "--bundle",
        ])
        .unwrap();
        match cli.command {
            Some(CliCommand::Doctor {
                deep,
                self_heal,
                snapshot,
                snapshot_path,
                bundle,
            }) => {
                assert!(deep);
                assert!(!self_heal);
                assert!(snapshot);
                assert!(bundle);
                assert_eq!(snapshot_path.as_deref(), Some("/tmp/doctor.json"));
            }
            _ => panic!("Expected Doctor command"),
        }
    }

    #[test]
    fn cli_parse_verify_provenance() {
        let cli = Cli::try_parse_from(vec![
            "hermes",
            "verify-provenance",
            "/tmp/doctor.json",
            "--signature",
            "/tmp/doctor.sig.json",
            "--strict",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(CliCommand::VerifyProvenance {
                path,
                signature,
                strict,
                json,
            }) => {
                assert_eq!(path, "/tmp/doctor.json");
                assert_eq!(signature.as_deref(), Some("/tmp/doctor.sig.json"));
                assert!(strict);
                assert!(json);
            }
            _ => panic!("expected verify-provenance command"),
        }
    }

    #[test]
    fn cli_parse_rotate_provenance_key() {
        let cli = Cli::try_parse_from(vec!["hermes", "rotate-provenance-key", "--json"]).unwrap();
        match cli.command {
            Some(CliCommand::RotateProvenanceKey { json }) => assert!(json),
            _ => panic!("expected rotate-provenance-key command"),
        }
    }

    #[test]
    fn cli_parse_route_learning_reset() {
        let cli = Cli::try_parse_from(vec!["hermes", "route-learning", "reset", "--json"]).unwrap();
        match cli.command {
            Some(CliCommand::RouteLearning { action, json }) => {
                assert_eq!(action.as_deref(), Some("reset"));
                assert!(json);
            }
            _ => panic!("expected route-learning command"),
        }
    }

    #[test]
    fn cli_parse_status() {
        let cli = Cli::try_parse_from(vec!["hermes", "status"]).unwrap();
        assert!(matches!(cli.command, Some(CliCommand::Status)));
    }

    #[test]
    fn cli_parse_elite_check() {
        let cli = Cli::try_parse_from(vec!["hermes", "elite-check", "--json", "--strict"]).unwrap();
        match cli.command {
            Some(CliCommand::EliteCheck { json, strict }) => {
                assert!(json);
                assert!(strict);
            }
            _ => panic!("Expected EliteCheck command"),
        }
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
            Some(CliCommand::Profile { action, name, .. }) => {
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

    #[test]
    fn cli_parse_secrets_set() {
        let cli = Cli::try_parse_from(vec![
            "hermes-agent-ultra",
            "secrets",
            "set",
            "openai",
            "--value",
            "sk-test",
        ])
        .unwrap();
        match cli.command {
            Some(CliCommand::Secrets {
                action,
                provider,
                value,
                show,
            }) => {
                assert_eq!(action.as_deref(), Some("set"));
                assert_eq!(provider.as_deref(), Some("openai"));
                assert_eq!(value.as_deref(), Some("sk-test"));
                assert!(!show);
            }
            _ => panic!("Expected Secrets command"),
        }
    }
}
