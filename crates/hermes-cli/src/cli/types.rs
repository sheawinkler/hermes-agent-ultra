//! CLI value types shared by the staged parser.

/// Effective Hermes subcommand after parsing.
#[derive(Debug, Clone)]
pub enum CliCommand {
    Hermes,
    Model {
        provider_model: Option<String>,
    },
    Tools {
        action: Option<String>,
        name: Option<String>,
        platform: Option<String>,
        summary: bool,
    },
    Config {
        action: Option<String>,
        key: Option<String>,
        value: Option<String>,
    },
    Gateway {
        action: Option<String>,
        system: bool,
        all: bool,
        force: bool,
        run_as_user: Option<String>,
        replace: bool,
        dry_run: bool,
        yes: bool,
        deep: bool,
    },
    Setup,
    Doctor {
        deep: bool,
        self_heal: bool,
        snapshot: bool,
        snapshot_path: Option<String>,
        bundle: bool,
    },
    Update {
        check: bool,
    },
    EliteCheck {
        json: bool,
        strict: bool,
    },
    VerifyProvenance {
        path: String,
        signature: Option<String>,
        strict: bool,
        json: bool,
    },
    RotateProvenanceKey {
        json: bool,
    },
    RouteLearning {
        action: Option<String>,
        json: bool,
    },
    RouteHealth {
        action: Option<String>,
        json: bool,
    },
    RouteAutotune {
        action: Option<String>,
        apply: bool,
        strict: bool,
        json: bool,
    },
    IncidentPack {
        snapshot: Option<String>,
        output: Option<String>,
        json: bool,
    },
    Status,
    Dashboard {
        host: String,
        port: u16,
        no_open: bool,
        insecure: bool,
    },
    Debug {
        action: Option<String>,
        url: Option<String>,
        lines: u32,
        expire: u32,
        local: bool,
    },
    Logs {
        lines: u32,
        follow: bool,
    },
    Profile {
        action: Option<String>,
        name: Option<String>,
        secondary: Option<String>,
        output: Option<String>,
        import_name: Option<String>,
        alias_name: Option<String>,
        remove: bool,
        yes: bool,
        clone: bool,
        clone_all: bool,
        clone_from: Option<String>,
        no_alias: bool,
        no_skills: bool,
    },
    Auth {
        action: Option<String>,
        provider: Option<String>,
        target: Option<String>,
        auth_type: Option<String>,
        label: Option<String>,
        api_key: Option<String>,
        qr: bool,
    },
    Secrets {
        action: Option<String>,
        provider: Option<String>,
        value: Option<String>,
        show: bool,
    },
    Cron {
        action: Option<String>,
        job_id: Option<String>,
        id: Option<String>,
        schedule: Option<String>,
        prompt: Option<String>,
        name: Option<String>,
        deliver: Option<String>,
        repeat: Option<u32>,
        skills: Vec<String>,
        add_skills: Vec<String>,
        remove_skills: Vec<String>,
        clear_skills: bool,
        script: Option<String>,
        no_agent: bool,
        agent: bool,
        script_timeout_seconds: Option<u64>,
        script_shell: Option<String>,
        all: bool,
    },
    Webhook {
        action: Option<String>,
        name: Option<String>,
        url: Option<String>,
        id: Option<String>,
        prompt: Option<String>,
        events: Option<String>,
        description: Option<String>,
        skills: Option<String>,
        deliver: Option<String>,
        deliver_chat_id: Option<String>,
        secret: Option<String>,
        deliver_only: bool,
        payload: Option<String>,
    },
    Chat {
        query: Option<String>,
        preload_skill: Option<String>,
        yolo: bool,
    },
    Skills {
        action: Option<String>,
        name: Option<String>,
        extra: Option<String>,
    },
    Plugins {
        action: Option<String>,
        name: Option<String>,
        git_ref: Option<String>,
        allow_untrusted_git_host: bool,
    },
    Memory {
        action: Option<String>,
        target: Option<String>,
        yes: bool,
    },
    Mcp {
        action: Option<String>,
        name: Option<String>,
        server: Option<String>,
        url: Option<String>,
        command: Option<String>,
        parallel_tools: bool,
    },
    Sessions {
        action: Option<String>,
        id: Option<String>,
        name: Option<String>,
    },
    Resume {
        session_id: Option<String>,
    },
    Insights {
        days: u32,
        source: Option<String>,
    },
    Login {
        provider: Option<String>,
    },
    Logout {
        provider: Option<String>,
    },
    Whatsapp {
        action: Option<String>,
    },
    Pairing {
        action: Option<String>,
        device_id: Option<String>,
    },
    Claw {
        action: Option<String>,
    },
    Acp {
        action: Option<String>,
    },
    Backup {
        output: Option<String>,
    },
    Import {
        path: String,
    },
    Version,
    Dump {
        session: Option<String>,
        output: Option<String>,
    },
    Completion {
        shell: Option<String>,
    },
    Uninstall {
        yes: bool,
    },
    Lumio {
        action: Option<String>,
        model: Option<String>,
    },
    /// `hermes meeting <action> [options]`
    ///
    /// Actions:
    /// - `record [--mode offline|realtime] [--title "..."]` — start live recording
    /// - `notes --audio <path> [--title "..."]`             — process audio file
    Meeting {
        action: Option<String>,
        /// Path to audio file (for `notes` action).
        audio: Option<String>,
        /// Meeting title (used in transcript filename and memory tags).
        title: Option<String>,
        /// Transcription mode override: `offline` or `realtime`.
        mode: Option<String>,
        /// Enable pyannote speaker diarization.
        diarize: bool,
    },
    PluginExternal(Vec<String>),
}

/// Hermes Agent CLI.
#[derive(Debug, Clone)]
pub struct Cli {
    pub command: Option<CliCommand>,
    pub verbose: bool,
    pub config_dir: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub oneshot: Option<String>,
    pub allow_tools: bool,
    pub personality: Option<String>,
    pub ignore_user_config: bool,
    pub ignore_rules: bool,
    pub accept_hooks: bool,
}

impl Cli {
    pub fn effective_command(&self) -> CliCommand {
        self.command.clone().unwrap_or(CliCommand::Hermes)
    }
}
