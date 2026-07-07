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
        /// Print provider:model completion candidates, one per line.
        #[arg(long, hide = true, conflicts_with = "completion_providers")]
        completion_values: bool,
        /// Print provider completion candidates, one per line.
        #[arg(long, hide = true)]
        completion_providers: bool,
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

    /// Manage the Computer Use cua-driver backend.
    #[command(name = "computer-use")]
    ComputerUse {
        /// Action: status, doctor, manifest, install-hint.
        action: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Run only named doctor checks. Repeat for multiple checks.
        #[arg(long = "include")]
        include: Vec<String>,
        /// Skip named doctor checks. Repeat for multiple checks.
        #[arg(long = "skip")]
        skip: Vec<String>,
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
        /// Compatibility flag for stale upstream docs; gateway lifecycle commands operate on all configured adapters.
        #[arg(long, hide = true)]
        platform: Option<String>,
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

    /// Ensure the always-on Hermes service surface is installed/running when supported.
    Up {
        /// Reinstall/overwrite the user service definition when supported.
        #[arg(long)]
        force: bool,
        /// Show the service plan without writing service files or starting anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run the interactive setup wizard.
    Setup {
        /// One-shot Nous Portal OAuth setup.
        #[arg(long)]
        portal: bool,
    },

    /// Nous Portal OAuth setup and status.
    ///
    /// Examples:
    ///   hermes portal          — run Nous Portal device-code login
    ///   hermes portal setup    — run Nous Portal device-code login
    ///   hermes portal info     — show Nous Portal auth status
    Portal {
        /// Action: "setup", "login", "info", or "status".
        action: Option<String>,
    },

    /// Nous Portal billing overview and explicit-confirmation billing actions.
    Billing {
        /// Billing action and arguments. Use `hermes billing help`.
        #[arg(
            value_name = "ARGS",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        args: Vec<String>,
    },

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
    Update {
        /// Compatibility flag for upstream parity (`hermes update --check`).
        #[arg(long)]
        check: bool,
    },

    /// Run consolidated elite diagnostics and release gates.
    EliteCheck {
        /// Print machine-readable JSON only.
        #[arg(long)]
        json: bool,
        /// Return non-zero on any failing elite gate.
        #[arg(long)]
        strict: bool,
    },

    /// Inspect implemented system surfaces: release, replay, MCP, ACP, handoff, providers, provenance.
    Systems {
        /// Action: status/release/replay/mcp/acp/providers/handoff/provenance/agent-card.
        action: Option<String>,
        /// Optional action topic (e.g. mcp conformance, acp conformance, handoff template).
        topic: Option<String>,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Write the JSON report/card to this path.
        #[arg(long)]
        output: Option<String>,
        /// Host for `systems agent-card serve`.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port for `systems agent-card serve`.
        #[arg(long, default_value_t = 9127)]
        port: u16,
        /// Serve one agent-card HTTP request, then exit.
        #[arg(long)]
        once: bool,
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

    /// Inspect computed smart-router health tiers from learned route telemetry.
    RouteHealth {
        /// Action: show/list/reset/clear
        action: Option<String>,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Compute and optionally apply smart-routing policy overrides from learned route health.
    RouteAutotune {
        /// Action: show/plan/inspect/apply/reset/clear
        action: Option<String>,
        /// Persist generated overrides to route-autotune.env.
        #[arg(long)]
        apply: bool,
        /// Fail with non-zero status when health evidence is insufficient.
        #[arg(long)]
        strict: bool,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Build an incident support bundle with replay/provenance diagnostics.
    IncidentPack {
        /// Optional existing doctor snapshot path.
        #[arg(long)]
        snapshot: Option<String>,
        /// Optional output path override (.tar.gz).
        #[arg(long)]
        output: Option<String>,
        /// Print machine-readable JSON summary.
        #[arg(long)]
        json: bool,
    },

    /// Show running status (active sessions, model, uptime).
    Status,

    /// Manage the local Kanban board without starting an interactive session.
    Kanban {
        /// Kanban action and arguments. Use `hermes kanban help`.
        #[arg(
            value_name = "ARGS",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        args: Vec<String>,
    },

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

    /// Start the headless dashboard-compatible HTTP API helper.
    Serve {
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
        /// Do not carry skill overrides from cloned source profiles.
        #[arg(long = "no-skills")]
        no_skills: bool,
    },

    /// Authentication management.
    Auth {
        /// Action: "login", "logout", "status", "verify", "add", "list", "remove", or "reset".
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
        /// Show full secret value for `get` (requires `HERMES_ALLOW_SECRET_STDOUT=1`; default masks output).
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
        /// Delivery chat/channel id for cron result routing.
        #[arg(long = "deliver-chat-id")]
        deliver_chat_id: Option<String>,
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
        /// Run in script-only mode (skip LLM/agent loop). Requires `--script`.
        #[arg(long)]
        no_agent: bool,
        /// Re-enable agent execution mode when editing a script-only job.
        #[arg(long)]
        agent: bool,
        /// Per-job script timeout in seconds (script/no-agent mode).
        #[arg(long)]
        script_timeout_seconds: Option<u64>,
        /// Per-job shell override for inline scripts.
        #[arg(long)]
        script_shell: Option<String>,
        /// Absolute working directory for this cron job.
        #[arg(long)]
        workdir: Option<String>,
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

    /// Cloudflare workflow helpers.
    Cloudflare {
        /// Action: parse-temporary-deploy-output.
        action: Option<String>,
        /// Run parser self-test.
        #[arg(long)]
        selftest: bool,
    },

    /// Microsoft Teams meeting summary pipeline.
    TeamsPipeline {
        /// Action: list/show/run/fetch/subscriptions/subscribe/renew-subscription/delete-subscription/maintain-subscriptions/token-health/validate.
        action: Option<String>,
        /// Job id or subscription id, depending on action.
        id: Option<String>,
        /// List limit.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Filter jobs by status.
        #[arg(long)]
        status: Option<String>,
        /// Override teams pipeline JSON store path.
        #[arg(long)]
        store_path: Option<String>,
        /// Meeting id for fetch.
        #[arg(long)]
        meeting_id: Option<String>,
        /// Join URL for fetch.
        #[arg(long)]
        join_web_url: Option<String>,
        /// Tenant id for fetch/job creation.
        #[arg(long)]
        tenant_id: Option<String>,
        /// Optional call record id for fetch.
        #[arg(long)]
        call_record_id: Option<String>,
        /// Graph subscription resource for subscribe.
        #[arg(long)]
        resource: Option<String>,
        /// Graph notification URL for subscribe.
        #[arg(long)]
        notification_url: Option<String>,
        /// Graph change type for subscribe.
        #[arg(long)]
        change_type: Option<String>,
        /// Graph subscription expiration timestamp.
        #[arg(long)]
        expiration: Option<String>,
        /// Graph subscription clientState.
        #[arg(long)]
        client_state: Option<String>,
        /// Graph lifecycle notification URL.
        #[arg(long)]
        lifecycle_notification_url: Option<String>,
        /// Latest supported TLS version for Graph subscriptions.
        #[arg(long, default_value = "v1_2")]
        latest_supported_tls_version: String,
        /// Force a fresh token for token-health.
        #[arg(long)]
        force_refresh: bool,
        /// Renewal threshold for maintain-subscriptions.
        #[arg(long, default_value_t = 24)]
        renew_within_hours: u32,
        /// Renewal extension for maintain-subscriptions.
        #[arg(long, default_value_t = 24)]
        extend_hours: u32,
        /// Dry-run maintain-subscriptions.
        #[arg(long)]
        dry_run: bool,
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
        /// Action: browse/search/install/inspect/list/check/update/audit/uninstall/publish/snapshot/tap/config/reset/subscribe/sync/opt-out/opt-in
        action: Option<String>,
        /// Skill name or search query.
        name: Option<String>,
        /// Additional argument (e.g. tap URL, snapshot path).
        #[arg(long)]
        extra: Option<String>,
        /// Also delete pristine bundled skills for `skills opt-out`.
        #[arg(long)]
        remove: bool,
        /// Skip confirmation for destructive skills maintenance actions.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Re-seed bundled skills immediately for `skills opt-in`.
        #[arg(long)]
        sync: bool,
    },

    /// Plugin management.
    Plugins {
        /// Action: install/update/remove/list/enable/disable/inspect
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
        /// Provider for `memory setup` (e.g. mem0) or reset target (all|memory|user).
        target: Option<String>,
        /// Skip reset confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Memory provider setup mode, currently used by Mem0: platform|selfhosted|oss.
        #[arg(long)]
        mode: Option<String>,
        /// Self-hosted memory provider host, currently used by Mem0.
        #[arg(long)]
        host: Option<String>,
        /// Memory provider API key, currently written to `$HERMES_HOME/.env` for Mem0.
        #[arg(long = "api-key")]
        api_key: Option<String>,
        /// Show the provider setup plan without writing config or env files.
        #[arg(long)]
        dry_run: bool,
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
        /// Mark this MCP server as supporting parallel tool calls.
        #[arg(long)]
        parallel_tools: bool,
    },

    /// Session management.
    Sessions {
        /// Action: list/export/delete/prune/optimize/repair/stats/rename/browse
        action: Option<String>,
        /// Session ID/prefix for export/delete/rename, export output path, or day count for prune.
        session: Option<String>,
        /// Session ID.
        #[arg(long)]
        id: Option<String>,
        /// Session ID/prefix for export, matching upstream `sessions export --session-id`.
        #[arg(long = "session-id")]
        session_id: Option<String>,
        /// New name (for rename).
        #[arg(long)]
        name: Option<String>,
        /// Export format: json, jsonl, md, markdown, or html.
        #[arg(long)]
        format: Option<String>,
        /// Export a filtered view, currently `user-prompts`.
        #[arg(long)]
        only: Option<String>,
        /// Export output path. Use `-` for stdout where supported.
        #[arg(long)]
        output: Option<String>,
        /// Redact common secret fields and token-like strings in exported content.
        #[arg(long)]
        redact: bool,
        /// Confirm destructive actions without prompting.
        #[arg(long)]
        yes: bool,
        /// Restrict prune to sessions with this source marker.
        #[arg(long)]
        source: Option<String>,
        /// Prune sessions older than N days.
        #[arg(long = "older-than")]
        older_than: Option<u64>,
    },

    /// Resume an interactive session from saved session state.
    Resume {
        /// Session ID/file stem to resume, or `latest` (default).
        session_id: Option<String>,
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
        /// Action: start/status/stop/restart/check/setup/setup-browser/version
        action: Option<String>,
        /// Run ACP startup checks without starting the server.
        #[arg(long, conflicts_with_all = ["action", "setup", "setup_browser"])]
        check: bool,
        /// Run Hermes model/provider setup for ACP clients.
        #[arg(long, conflicts_with_all = ["action", "check", "setup_browser"])]
        setup: bool,
        /// Check browser tooling needed by ACP browser workflows.
        #[arg(long = "setup-browser", conflicts_with_all = ["action", "check", "setup"])]
        setup_browser: bool,
        /// Print version information without starting the ACP server.
        #[arg(
            long,
            conflicts_with_all = ["action", "check", "setup", "setup_browser"]
        )]
        version: bool,
        /// Assume yes / non-interactive mode for setup subcommands.
        #[arg(long)]
        yes: bool,
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

    /// Reserved external command surface.
    ///
    /// Hermes Agent Ultra is Rust-only at runtime, so external Python plugin
    /// command bridges are rejected instead of being executed dynamically.
    #[command(external_subcommand)]
    PluginExternal(Vec<String>),
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

    /// Force-enable tools in one-shot/query mode (`-z` / `chat --query`).
    ///
    /// Tools are enabled by default in query mode. Use
    /// `HERMES_QUERY_DISABLE_TOOLS=1` to force-disable.
    #[arg(long, global = true)]
    pub allow_tools: bool,

    /// Override the personality / persona.
    #[arg(short = 'p', long, global = true)]
    pub personality: Option<String>,

    /// Ignore user config files (`config.yaml`, `cli-config.yaml`, `gateway.json`) for this run.
    #[arg(long, global = true)]
    pub ignore_user_config: bool,

    /// Ignore local instruction/rules context injection (AGENTS.md/SOUL.md/etc.) for this run.
    #[arg(long, global = true)]
    pub ignore_rules: bool,
}

impl Cli {
    /// Return the effective command, defaulting to `CliCommand::Hermes`.
    pub fn effective_command(&self) -> CliCommand {
        self.command.clone().unwrap_or(CliCommand::Hermes)
    }
}

#[cfg(test)]
mod tests;
