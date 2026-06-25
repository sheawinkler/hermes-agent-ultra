//! Per-command argument structs and second-pass parsers.

use std::ffi::OsString;

use clap::{CommandFactory, FromArgMatches, Parser};

use super::globals;
use super::types::CliCommand;

fn parse_subcommand<A, F>(args: &[OsString], map: F) -> Result<CliCommand, clap::Error>
where
    A: CommandFactory + FromArgMatches,
    F: FnOnce(A) -> CliCommand,
{
    let cmd = globals::command_with_subcommand(A::command());
    let matches = cmd.try_get_matches_from(args)?;
    let Some((_, sub)) = matches.subcommand() else {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::MissingSubcommand,
            "missing subcommand",
        ));
    };
    Ok(map(A::from_arg_matches(sub)?))
}

#[derive(Parser, Debug, Clone)]
#[command(name = "model", about = "Show or set the current model")]
struct ModelArgs {
    provider_model: Option<String>,
}

pub fn parse_model(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ModelArgs, _>(args, |a| CliCommand::Model {
        provider_model: a.provider_model,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "tools", about = "tools command")]
struct ToolsArgs {
    action: Option<String>,
    name: Option<String>,
    platform: Option<String>,
    #[arg(long)]
    summary: bool,
}

pub fn parse_tools(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ToolsArgs, _>(args, |a| CliCommand::Tools {
        action: a.action,
        name: a.name,
        platform: a.platform,
        summary: a.summary,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "config", about = "config command")]
struct ConfigArgs {
    action: Option<String>,
    key: Option<String>,
    value: Option<String>,
}

pub fn parse_config(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ConfigArgs, _>(args, |a| CliCommand::Config {
        action: a.action,
        key: a.key,
        value: a.value,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "gateway", about = "Start or manage the gateway server")]
struct GatewayArgs {
    action: Option<String>,
    #[arg(long)]
    system: bool,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    run_as_user: Option<String>,
    #[arg(long)]
    replace: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(short = 'y', long)]
    yes: bool,
    #[arg(long)]
    deep: bool,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "setup", about = "Run the interactive setup wizard")]
struct SetupArgs {
    #[arg(long)]
    portal: bool,
}

pub fn parse_setup(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<SetupArgs, _>(args, |a| CliCommand::Setup { portal: a.portal })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "portal", about = "Nous Portal OAuth setup and status")]
struct PortalArgs {
    action: Option<String>,
}

pub fn parse_portal(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<PortalArgs, _>(args, |a| CliCommand::Portal { action: a.action })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "systems", about = "Inspect implemented system surfaces")]
struct SystemsArgs {
    action: Option<String>,
    topic: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    output: Option<String>,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 9127)]
    port: u16,
    #[arg(long)]
    once: bool,
}

pub fn parse_systems(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<SystemsArgs, _>(args, |a| CliCommand::Systems {
        action: a.action,
        topic: a.topic,
        json: a.json,
        output: a.output,
        host: a.host,
        port: a.port,
        once: a.once,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "kanban", about = "Manage the local Kanban board")]
struct KanbanArgs {
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    args: Vec<String>,
}

pub fn parse_kanban(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<KanbanArgs, _>(args, |a| CliCommand::Kanban { args: a.args })
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "teams-pipeline",
    about = "Microsoft Teams meeting summary pipeline"
)]
struct TeamsPipelineArgs {
    action: Option<String>,
    id: Option<String>,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    status: Option<String>,
    #[arg(long)]
    store_path: Option<String>,
    #[arg(long)]
    meeting_id: Option<String>,
    #[arg(long)]
    join_web_url: Option<String>,
    #[arg(long)]
    tenant_id: Option<String>,
    #[arg(long)]
    call_record_id: Option<String>,
    #[arg(long)]
    resource: Option<String>,
    #[arg(long)]
    notification_url: Option<String>,
    #[arg(long)]
    change_type: Option<String>,
    #[arg(long)]
    expiration: Option<String>,
    #[arg(long)]
    client_state: Option<String>,
    #[arg(long)]
    lifecycle_notification_url: Option<String>,
    #[arg(long, default_value = "v1_2")]
    latest_supported_tls_version: String,
    #[arg(long)]
    force_refresh: bool,
    #[arg(long, default_value_t = 24)]
    renew_within_hours: u32,
    #[arg(long, default_value_t = 24)]
    extend_hours: u32,
    #[arg(long)]
    dry_run: bool,
}

pub fn parse_teams_pipeline(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<TeamsPipelineArgs, _>(args, |a| CliCommand::TeamsPipeline {
        action: a.action,
        id: a.id,
        limit: a.limit,
        status: a.status,
        store_path: a.store_path,
        meeting_id: a.meeting_id,
        join_web_url: a.join_web_url,
        tenant_id: a.tenant_id,
        call_record_id: a.call_record_id,
        resource: a.resource,
        notification_url: a.notification_url,
        change_type: a.change_type,
        expiration: a.expiration,
        client_state: a.client_state,
        lifecycle_notification_url: a.lifecycle_notification_url,
        latest_supported_tls_version: a.latest_supported_tls_version,
        force_refresh: a.force_refresh,
        renew_within_hours: a.renew_within_hours,
        extend_hours: a.extend_hours,
        dry_run: a.dry_run,
    })
}

pub fn parse_gateway(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<GatewayArgs, _>(args, |a| CliCommand::Gateway {
        action: a.action,
        system: a.system,
        all: a.all,
        force: a.force,
        run_as_user: a.run_as_user,
        replace: a.replace,
        dry_run: a.dry_run,
        yes: a.yes,
        deep: a.deep,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "doctor", about = "doctor command")]
struct DoctorArgs {
    #[arg(long)]
    deep: bool,
    #[arg(long)]
    self_heal: bool,
    #[arg(long)]
    snapshot: bool,
    #[arg(long)]
    snapshot_path: Option<String>,
    #[arg(long)]
    bundle: bool,
}

pub fn parse_doctor(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<DoctorArgs, _>(args, |a| CliCommand::Doctor {
        deep: a.deep,
        self_heal: a.self_heal,
        snapshot: a.snapshot,
        snapshot_path: a.snapshot_path,
        bundle: a.bundle,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "update",
    about = "Check for updates or perform OTA self-update"
)]
struct UpdateArgs {
    /// Only check for updates without installing
    #[arg(long)]
    check: bool,
    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    yes: bool,
    /// Rollback to previous version
    #[arg(long)]
    rollback: bool,
    /// Force update even if already on latest version
    #[arg(long)]
    force: bool,
    /// Force update source: "github" or "modelscope"
    #[arg(long, value_parser = ["github", "modelscope"])]
    source: Option<String>,
    /// Update channel: "stable", "beta", "rc", "nightly"
    #[arg(long, value_parser = ["stable", "beta", "rc", "nightly"])]
    channel: Option<String>,
}

pub fn parse_update(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<UpdateArgs, _>(args, |a| CliCommand::Update {
        check: a.check,
        yes: a.yes,
        rollback: a.rollback,
        force: a.force,
        source: a.source,
        channel: a.channel,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "elite-check", about = "elite check command")]
struct EliteCheckArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    strict: bool,
}

pub fn parse_elite_check(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<EliteCheckArgs, _>(args, |a| CliCommand::EliteCheck {
        json: a.json,
        strict: a.strict,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "verify-provenance", about = "verify provenance command")]
struct VerifyProvenanceArgs {
    path: String,
    #[arg(long)]
    signature: Option<String>,
    #[arg(long)]
    strict: bool,
    #[arg(long)]
    json: bool,
}

pub fn parse_verify_provenance(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<VerifyProvenanceArgs, _>(args, |a| CliCommand::VerifyProvenance {
        path: a.path,
        signature: a.signature,
        strict: a.strict,
        json: a.json,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "rotate-provenance-key",
    about = "rotate provenance key command"
)]
struct RotateProvenanceKeyArgs {
    #[arg(long)]
    json: bool,
}

pub fn parse_rotate_provenance_key(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<RotateProvenanceKeyArgs, _>(args, |a| CliCommand::RotateProvenanceKey {
        json: a.json,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "route-learning", about = "route learning command")]
struct RouteLearningArgs {
    action: Option<String>,
    #[arg(long)]
    json: bool,
}

pub fn parse_route_learning(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<RouteLearningArgs, _>(args, |a| CliCommand::RouteLearning {
        action: a.action,
        json: a.json,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "route-health", about = "route health command")]
struct RouteHealthArgs {
    action: Option<String>,
    #[arg(long)]
    json: bool,
}

pub fn parse_route_health(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<RouteHealthArgs, _>(args, |a| CliCommand::RouteHealth {
        action: a.action,
        json: a.json,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "route-autotune", about = "route autotune command")]
struct RouteAutotuneArgs {
    action: Option<String>,
    #[arg(long)]
    apply: bool,
    #[arg(long)]
    strict: bool,
    #[arg(long)]
    json: bool,
}

pub fn parse_route_autotune(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<RouteAutotuneArgs, _>(args, |a| CliCommand::RouteAutotune {
        action: a.action,
        apply: a.apply,
        strict: a.strict,
        json: a.json,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "incident-pack", about = "incident pack command")]
struct IncidentPackArgs {
    #[arg(long)]
    snapshot: Option<String>,
    #[arg(long)]
    output: Option<String>,
    #[arg(long)]
    json: bool,
}

pub fn parse_incident_pack(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<IncidentPackArgs, _>(args, |a| CliCommand::IncidentPack {
        snapshot: a.snapshot,
        output: a.output,
        json: a.json,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "dashboard", about = "dashboard command")]
struct DashboardArgs {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 9119)]
    port: u16,
    #[arg(long)]
    no_open: bool,
    #[arg(long)]
    insecure: bool,
}

pub fn parse_dashboard(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<DashboardArgs, _>(args, |a| CliCommand::Dashboard {
        host: a.host,
        port: a.port,
        no_open: a.no_open,
        insecure: a.insecure,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "debug", about = "debug command")]
struct DebugArgs {
    action: Option<String>,
    url: Option<String>,
    #[arg(long, default_value_t = 200)]
    lines: u32,
    #[arg(long, default_value_t = 7)]
    expire: u32,
    #[arg(long)]
    local: bool,
}

pub fn parse_debug(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<DebugArgs, _>(args, |a| CliCommand::Debug {
        action: a.action,
        url: a.url,
        lines: a.lines,
        expire: a.expire,
        local: a.local,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "logs", about = "logs command")]
struct LogsArgs {
    #[arg(default_value = "20")]
    lines: u32,
    #[arg(short, long)]
    follow: bool,
}

pub fn parse_logs(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<LogsArgs, _>(args, |a| CliCommand::Logs {
        lines: a.lines,
        follow: a.follow,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "profile", about = "profile command")]
struct ProfileArgs {
    action: Option<String>,
    name: Option<String>,
    secondary: Option<String>,
    #[arg(short, long)]
    output: Option<String>,
    #[arg(long)]
    import_name: Option<String>,
    #[arg(long = "name")]
    alias_name: Option<String>,
    #[arg(long)]
    remove: bool,
    #[arg(short = 'y', long)]
    yes: bool,
    #[arg(long)]
    clone: bool,
    #[arg(long = "clone-all")]
    clone_all: bool,
    #[arg(long = "clone-from")]
    clone_from: Option<String>,
    #[arg(long = "no-alias")]
    no_alias: bool,
    #[arg(long = "no-skills")]
    no_skills: bool,
}

pub fn parse_profile(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ProfileArgs, _>(args, |a| CliCommand::Profile {
        action: a.action,
        name: a.name,
        secondary: a.secondary,
        output: a.output,
        import_name: a.import_name,
        alias_name: a.alias_name,
        remove: a.remove,
        yes: a.yes,
        clone: a.clone,
        clone_all: a.clone_all,
        clone_from: a.clone_from,
        no_alias: a.no_alias,
        no_skills: a.no_skills,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "auth", about = "auth command")]
struct AuthArgs {
    action: Option<String>,
    provider: Option<String>,
    target: Option<String>,
    #[arg(long = "type")]
    auth_type: Option<String>,
    #[arg(long)]
    label: Option<String>,
    #[arg(long = "api-key")]
    api_key: Option<String>,
    #[arg(long)]
    qr: bool,
}

pub fn parse_auth(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<AuthArgs, _>(args, |a| CliCommand::Auth {
        action: a.action,
        provider: a.provider,
        target: a.target,
        auth_type: a.auth_type,
        label: a.label,
        api_key: a.api_key,
        qr: a.qr,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "secrets", about = "secrets command")]
struct SecretsArgs {
    action: Option<String>,
    provider: Option<String>,
    #[arg(long)]
    value: Option<String>,
    #[arg(long)]
    show: bool,
}

pub fn parse_secrets(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<SecretsArgs, _>(args, |a| CliCommand::Secrets {
        action: a.action,
        provider: a.provider,
        value: a.value,
        show: a.show,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "cron", about = "cron command")]
struct CronArgs {
    action: Option<String>,
    job_id: Option<String>,
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    schedule: Option<String>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    deliver: Option<String>,
    #[arg(long)]
    repeat: Option<u32>,
    #[arg(long = "skill")]
    skills: Vec<String>,
    #[arg(long = "add-skill")]
    add_skills: Vec<String>,
    #[arg(long = "remove-skill")]
    remove_skills: Vec<String>,
    #[arg(long)]
    clear_skills: bool,
    #[arg(long)]
    script: Option<String>,
    #[arg(long)]
    no_agent: bool,
    #[arg(long)]
    agent: bool,
    #[arg(long)]
    script_timeout_seconds: Option<u64>,
    #[arg(long)]
    script_shell: Option<String>,
    #[arg(long)]
    all: bool,
}

pub fn parse_cron(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<CronArgs, _>(args, |a| CliCommand::Cron {
        action: a.action,
        job_id: a.job_id,
        id: a.id,
        schedule: a.schedule,
        prompt: a.prompt,
        name: a.name,
        deliver: a.deliver,
        repeat: a.repeat,
        skills: a.skills,
        add_skills: a.add_skills,
        remove_skills: a.remove_skills,
        clear_skills: a.clear_skills,
        script: a.script,
        no_agent: a.no_agent,
        agent: a.agent,
        script_timeout_seconds: a.script_timeout_seconds,
        script_shell: a.script_shell,
        all: a.all,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "webhook", about = "webhook command")]
struct WebhookArgs {
    action: Option<String>,
    name: Option<String>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    events: Option<String>,
    #[arg(long)]
    description: Option<String>,
    #[arg(long)]
    skills: Option<String>,
    #[arg(long)]
    deliver: Option<String>,
    #[arg(long = "deliver-chat-id")]
    deliver_chat_id: Option<String>,
    #[arg(long)]
    secret: Option<String>,
    #[arg(long = "deliver-only")]
    deliver_only: bool,
    #[arg(long)]
    payload: Option<String>,
}

pub fn parse_webhook(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<WebhookArgs, _>(args, |a| CliCommand::Webhook {
        action: a.action,
        name: a.name,
        url: a.url,
        id: a.id,
        prompt: a.prompt,
        events: a.events,
        description: a.description,
        skills: a.skills,
        deliver: a.deliver,
        deliver_chat_id: a.deliver_chat_id,
        secret: a.secret,
        deliver_only: a.deliver_only,
        payload: a.payload,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "chat", about = "chat command")]
struct ChatArgs {
    #[arg(short, long)]
    query: Option<String>,
    #[arg(long)]
    preload_skill: Option<String>,
    #[arg(long)]
    yolo: bool,
    #[arg(
        long,
        help = "Enable plan-then-execute mode (read-only planning until approved)"
    )]
    plan: bool,
}

pub fn parse_chat(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ChatArgs, _>(args, |a| CliCommand::Chat {
        query: a.query,
        preload_skill: a.preload_skill,
        yolo: a.yolo,
        plan: a.plan,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "skills", about = "skills command")]
struct SkillsArgs {
    action: Option<String>,
    name: Option<String>,
    #[arg(long)]
    extra: Option<String>,
}

pub fn parse_skills(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<SkillsArgs, _>(args, |a| CliCommand::Skills {
        action: a.action,
        name: a.name,
        extra: a.extra,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "plugins", about = "plugins command")]
struct PluginsArgs {
    action: Option<String>,
    name: Option<String>,
    #[arg(long = "ref")]
    git_ref: Option<String>,
    #[arg(long)]
    allow_untrusted_git_host: bool,
}

pub fn parse_plugins(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<PluginsArgs, _>(args, |a| CliCommand::Plugins {
        action: a.action,
        name: a.name,
        git_ref: a.git_ref,
        allow_untrusted_git_host: a.allow_untrusted_git_host,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "memory", about = "memory command")]
struct MemoryArgs {
    action: Option<String>,
    target: Option<String>,
    #[arg(short = 'y', long)]
    yes: bool,
}

pub fn parse_memory(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<MemoryArgs, _>(args, |a| CliCommand::Memory {
        action: a.action,
        target: a.target,
        yes: a.yes,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "interest", about = "local user interest (POI) topics")]
struct InterestArgs {
    action: Option<String>,
    /// Set extract mode when using `enable` (`rules`, `hybrid`, `llm`).
    #[arg(long, value_name = "MODE")]
    mode: Option<String>,
    /// Enable session-end auxiliary LLM extraction when using `enable`.
    #[arg(long)]
    llm_on_session_end: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    rest: Vec<String>,
}

pub fn parse_interest(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<InterestArgs, _>(args, |a| CliCommand::Interest {
        action: a.action,
        mode: a.mode,
        llm_on_session_end: a.llm_on_session_end,
        rest: a.rest,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "contribute",
    about = "De-identified POI/skills contribution to ops server (opt-in)"
)]
struct ContributeArgs {
    action: Option<String>,
    #[arg(long, help = "Only toggle POI/interest upload")]
    poi_only: bool,
    #[arg(long, help = "Only toggle skills pattern upload")]
    skills_only: bool,
    #[arg(long, help = "Preview using last-session style snapshot")]
    last_session: bool,
    #[arg(
        long,
        help = "With reset: delete all outbox rows instead of requeueing sent/failed"
    )]
    outbox_clear: bool,
}

pub fn parse_contribute(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ContributeArgs, _>(args, |a| CliCommand::Contribute {
        action: a.action,
        poi_only: a.poi_only,
        skills_only: a.skills_only,
        last_session: a.last_session,
        outbox_clear: a.outbox_clear,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "server",
    about = "Remote LLM server account (login/logout/whoami/profile/balance/checkin/doctor)"
)]
struct ServerArgs {
    action: Option<String>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    rest: Vec<String>,
    #[arg(long, value_parser = ["wechat", "wechat_qr", "email", "email_otp"])]
    method: Option<String>,
}

pub fn parse_server(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ServerArgs, _>(args, |a| CliCommand::Server {
        action: a.action,
        rest: a.rest,
        method: a.method,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(
    name = "media",
    about = "Image/video generation setup (models, workflows, config)"
)]
struct MediaArgs {
    action: Option<String>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    rest: Vec<String>,
}

pub fn parse_media(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<MediaArgs, _>(args, |a| CliCommand::Media {
        action: a.action,
        rest: a.rest,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "mcp", about = "mcp command")]
struct McpArgs {
    action: Option<String>,
    name: Option<String>,
    #[arg(long)]
    server: Option<String>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    command: Option<String>,
    #[arg(long)]
    parallel_tools: bool,
}

pub fn parse_mcp(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<McpArgs, _>(args, |a| CliCommand::Mcp {
        action: a.action,
        name: a.name,
        server: a.server,
        url: a.url,
        command: a.command,
        parallel_tools: a.parallel_tools,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "sessions", about = "sessions command")]
struct SessionsArgs {
    action: Option<String>,
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    name: Option<String>,
}

pub fn parse_sessions(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<SessionsArgs, _>(args, |a| CliCommand::Sessions {
        action: a.action,
        id: a.id,
        name: a.name,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "resume", about = "resume command")]
struct ResumeArgs {
    session_id: Option<String>,
}

pub fn parse_resume(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ResumeArgs, _>(args, |a| CliCommand::Resume {
        session_id: a.session_id,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "meeting", about = "Meeting recorder and notes generator")]
struct MeetingArgs {
    /// Action: `record` (live recording) or `notes` (process audio file).
    action: Option<String>,
    /// Path to audio file (required for `notes` action).
    #[arg(long)]
    audio: Option<String>,
    /// Meeting title (used for transcript filename and memory tags).
    #[arg(long)]
    title: Option<String>,
    /// Transcription mode: `offline` (default) or `realtime`.
    #[arg(long)]
    mode: Option<String>,
    /// Enable pyannote speaker diarization.
    #[arg(long)]
    diarize: bool,
}

pub fn parse_meeting(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<MeetingArgs, _>(args, |a| CliCommand::Meeting {
        action: a.action,
        audio: a.audio,
        title: a.title,
        mode: a.mode,
        diarize: a.diarize,
    })
}

#[cfg(any(feature = "talk", feature = "talk-rockchip"))]
#[derive(Parser, Debug, Clone)]
#[command(name = "talk", about = "Real-time voice dialog (ASR + LLM + TTS)")]
struct TalkArgs {
    /// Action: `run`, `init`, `list-devices`, `probe-capture`, `probe-playback`, `enroll`.
    action: Option<String>,
    /// Path to config.toml (default: `$HERMES_HOME/hermes-talk/config.toml`).
    #[arg(long)]
    config: Option<String>,
    /// Seconds for `probe-capture` / `enroll`.
    #[arg(long, default_value_t = 5)]
    seconds: u64,
}

#[cfg(any(feature = "talk", feature = "talk-rockchip"))]
pub fn parse_talk(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<TalkArgs, _>(args, |a| CliCommand::Talk {
        action: a.action,
        config: a.config,
        seconds: a.seconds,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "insights", about = "insights command")]
struct InsightsArgs {
    #[arg(long, default_value = "30")]
    days: u32,
    #[arg(long)]
    source: Option<String>,
}

pub fn parse_insights(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<InsightsArgs, _>(args, |a| CliCommand::Insights {
        days: a.days,
        source: a.source,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "login", about = "login command")]
struct LoginArgs {
    provider: Option<String>,
}

pub fn parse_login(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<LoginArgs, _>(args, |a| CliCommand::Login {
        provider: a.provider,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "logout", about = "logout command")]
struct LogoutArgs {
    provider: Option<String>,
}

pub fn parse_logout(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<LogoutArgs, _>(args, |a| CliCommand::Logout {
        provider: a.provider,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "whatsapp", about = "whatsapp command")]
struct WhatsappArgs {
    action: Option<String>,
}

pub fn parse_whatsapp(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<WhatsappArgs, _>(args, |a| CliCommand::Whatsapp { action: a.action })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "pairing", about = "pairing command")]
struct PairingArgs {
    action: Option<String>,
    #[arg()]
    args: Vec<String>,
    #[arg(long)]
    device_id: Option<String>,
}

pub fn parse_pairing(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<PairingArgs, _>(args, |a| CliCommand::Pairing {
        action: a.action,
        device_id: a.device_id,
        args: a.args,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "claw", about = "claw command")]
struct ClawArgs {
    action: Option<String>,
}

pub fn parse_claw(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ClawArgs, _>(args, |a| CliCommand::Claw { action: a.action })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "acp", about = "acp command")]
struct AcpArgs {
    action: Option<String>,
}

pub fn parse_acp(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<AcpArgs, _>(args, |a| CliCommand::Acp { action: a.action })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "backup", about = "backup command")]
struct BackupArgs {
    output: Option<String>,
}

pub fn parse_backup(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<BackupArgs, _>(args, |a| CliCommand::Backup { output: a.output })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "import", about = "import command")]
struct ImportArgs {
    path: String,
}

pub fn parse_import(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<ImportArgs, _>(args, |a| CliCommand::Import { path: a.path })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "dump", about = "dump command")]
struct DumpArgs {
    session: Option<String>,
    output: Option<String>,
}

pub fn parse_dump(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<DumpArgs, _>(args, |a| CliCommand::Dump {
        session: a.session,
        output: a.output,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "completion", about = "completion command")]
struct CompletionArgs {
    shell: Option<String>,
}

pub fn parse_completion(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<CompletionArgs, _>(args, |a| CliCommand::Completion { shell: a.shell })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "uninstall", about = "uninstall command")]
struct UninstallArgs {
    #[arg(long)]
    yes: bool,
}

pub fn parse_uninstall(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<UninstallArgs, _>(args, |a| CliCommand::Uninstall { yes: a.yes })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "lumio", about = "lumio command")]
struct LumioArgs {
    action: Option<String>,
    #[arg(short, long)]
    model: Option<String>,
}

pub fn parse_lumio(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<LumioArgs, _>(args, |a| CliCommand::Lumio {
        action: a.action,
        model: a.model,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(name = "_ensure-dep", about = "hidden runtime dependency installer")]
struct EnsureDepArgs {
    dep: String,
    #[arg(long)]
    quiet: bool,
}

pub fn parse_ensure_dep(args: &[OsString]) -> Result<CliCommand, clap::Error> {
    parse_subcommand::<EnsureDepArgs, _>(args, |a| CliCommand::EnsureDep {
        dep: a.dep,
        quiet: a.quiet,
    })
}

/// Fully-specified subcommands for shell completion (built on demand, not at startup).
pub fn all_subcommand_commands() -> Vec<clap::Command> {
    macro_rules! commands {
        ($($(#[$attr:meta])* $parser:ty),* $(,)?) => {
            vec![$($(#[$attr])* <$parser as CommandFactory>::command()),*]
        };
    }

    commands![
        ModelArgs,
        ToolsArgs,
        ConfigArgs,
        SetupArgs,
        PortalArgs,
        SystemsArgs,
        KanbanArgs,
        TeamsPipelineArgs,
        GatewayArgs,
        DoctorArgs,
        UpdateArgs,
        EliteCheckArgs,
        VerifyProvenanceArgs,
        RotateProvenanceKeyArgs,
        RouteLearningArgs,
        RouteHealthArgs,
        RouteAutotuneArgs,
        IncidentPackArgs,
        DashboardArgs,
        DebugArgs,
        LogsArgs,
        ProfileArgs,
        AuthArgs,
        SecretsArgs,
        CronArgs,
        WebhookArgs,
        ChatArgs,
        SkillsArgs,
        PluginsArgs,
        MemoryArgs,
        InterestArgs,
        ContributeArgs,
        ServerArgs,
        MediaArgs,
        McpArgs,
        MeetingArgs,
        #[cfg(any(feature = "talk", feature = "talk-rockchip"))]
        TalkArgs,
        SessionsArgs,
        ResumeArgs,
        InsightsArgs,
        LoginArgs,
        LogoutArgs,
        WhatsappArgs,
        PairingArgs,
        ClawArgs,
        AcpArgs,
        BackupArgs,
        ImportArgs,
        DumpArgs,
        CompletionArgs,
        UninstallArgs,
        LumioArgs,
    ]
}
