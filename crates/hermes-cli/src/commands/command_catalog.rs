// ---------------------------------------------------------------------------
// CommandResult
// ---------------------------------------------------------------------------

/// Result of handling a slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResult {
    /// The command was fully handled (no further action needed).
    Handled,
    /// The command requires the agent to process a follow-up message.
    NeedsAgent,
    /// The user requested to quit the application.
    Quit,
}

fn secret_stdout_allowed() -> bool {
    std::env::var("HERMES_ALLOW_SECRET_STDOUT")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn mask_secret_value(secret: &str) -> String {
    if secret.is_empty() {
        return "(empty)".to_string();
    }
    if secret.len() <= 8 {
        return "*".repeat(secret.len());
    }
    format!(
        "{}***{}",
        &secret[..4],
        &secret[secret.len().saturating_sub(4)..]
    )
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// All supported slash commands and their descriptions.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/start", "Acknowledge platform start pings without a reply"),
    ("/new", "Start a new session"),
    ("/reset", "Reset the current session (clear messages)"),
    (
        "/clear",
        "Clear screen/session state and start a fresh session",
    ),
    ("/retry", "Retry the last user message"),
    (
        "/prompt",
        "Stage a markdown prompt draft in the composer (`/prompt [initial text]`)",
    ),
    ("/compose", "Alias for /prompt"),
    ("/undo", "Undo the last N user turns and prefill the latest undone prompt"),
    ("/rewind", "Alias for /undo [N]"),
    ("/history", "Show recent conversation history"),
    (
        "/recap",
        "Summarize recent session activity (`/recap [count]`)",
    ),
    (
        "/context",
        "Context breakdown (`status|breakdown|compress`)",
    ),
    ("/title", "Set or show session title metadata"),
    ("/topic", "Session topic metadata controls"),
    (
        "/branch",
        "Create a branch/fork marker for the current session",
    ),
    ("/fork", "Alias for /branch"),
    (
        "/timetravel",
        "Session time-travel controls (`list|latest|goto <snapshot>|undo [n]|branch [label]`)",
    ),
    ("/tt", "Alias for /timetravel"),
    ("/snapshot", "Create/list snapshot checkpoints"),
    ("/snap", "Alias for /snapshot"),
    ("/rollback", "List rollback checkpoints"),
    (
        "/model",
        "Show/switch models, run capability diagnostics (`/model explain`, `why-not`, `harness`, `backend`), or configure failover (`/model failover`)",
    ),
    (
        "/codex-runtime",
        "Toggle Codex app-server runtime for OpenAI/Codex models (`auto|codex_app_server`)",
    ),
    ("/codex_runtime", "Alias for /codex-runtime"),
    (
        "/auth",
        "Auth lifecycle controls (`status|verify|refresh`) for active provider credentials",
    ),
    ("/provider", "List configured providers and availability"),
    (
        "/personality",
        "Show current personality, list built-ins, or switch mode",
    ),
    ("/profile", "Show active profile and Hermes home path"),
    ("/whoami", "Alias for /profile"),
    ("/version", "Show Hermes Agent Ultra version and build label"),
    ("/v", "Alias for /version"),
    ("/fast", "Toggle fast-mode hints"),
    (
        "/timestamps",
        "Toggle transcript timestamps (`/timestamps [on|off|status]`)",
    ),
    ("/ts", "Alias for /timestamps"),
    ("/skin", "Show available skin/theme options"),
    ("/skins", "Alias for /skin"),
    ("/voice", "Show voice mode status"),
    (
        "/pet",
        "Animated companion controls (`status|on|off|toggle|list|set|mood|dock|speed`)",
    ),
    (
        "/hatch",
        "Generate a petdex companion request from a description",
    ),
    ("/generate-pet", "Alias for /hatch"),
    ("/skills", "List available skills"),
    (
        "/learn",
        "Capture a reusable learning request and inject it into the next turn",
    ),
    ("/skill", "Alias for /skills"),
    (
        "/bundles",
        "List skill bundles (aliases that load multiple skills)",
    ),
    (
        "/curator",
        "Skill curator/control-plane compatibility surface",
    ),
    ("/tools", "List registered tools"),
    (
        "/toolcards",
        "Inline tool-card controls (e.g. `/toolcards export`)",
    ),
    ("/toolsets", "Show configured toolsets by platform"),
    ("/plugins", "List plugin bundles and status"),
    (
        "/memory",
        "Show memory backend status and pending write summary (`status|pending`)",
    ),
    (
        "/disk-cleanup",
        "Rust-native ephemeral file cleanup (`status|dry-run|quick|deep|track|forget`)",
    ),
    ("/mcp", "List configured MCP servers"),
    ("/reload", "Reload runtime env/config values"),
    ("/reload-skills", "Refresh installed skill index/registry"),
    ("/reload_skills", "Alias for /reload-skills"),
    ("/reload-mcp", "Reload MCP server metadata"),
    ("/reload_mcp", "Alias for /reload-mcp"),
    ("/cron", "Show cron scheduler status"),
    (
        "/blueprint",
        "Automation Blueprint catalog and creation (`<name> slot=value`, alias `/bp`)",
    ),
    ("/bp", "Alias for /blueprint"),
    (
        "/suggestions",
        "Review suggested automations (`accept|dismiss N`, `catalog`, `clear`)",
    ),
    ("/suggest", "Alias for /suggestions"),
    ("/scheduler", "Alias for /background"),
    (
        "/agents",
        "Show active/background task state (`status|pause|resume|doctor`)",
    ),
    ("/tasks", "Alias for /kanban"),
    ("/queue", "Queue a follow-up prompt"),
    ("/q", "Alias for /queue"),
    (
        "/handoff",
        "Queue a session handoff request to a configured gateway platform (`/handoff <platform>`)",
    ),
    (
        "/evolve",
        "Run or inspect the self-evolution intelligence loop",
    ),
    (
        "/subgoal",
        "Objective checklist controls (`show|<text>|complete|impossible|undo|remove|clear`)",
    ),
    (
        "/objective",
        "Set/show objective contract + profile/policies (`status|verify|plan|constraints|counterfactual|wait|unwait|profile|context|simulator|ensemble|ledger|dag|eval|clear`)",
    ),
    (
        "/claims",
        "Claim verifier controls (`status|on|off`) for verified/inferred/unproven final tagging",
    ),
    (
        "/quorum",
        "Optional multi-voter deep-reasoning mode (`status|on|off|models|run`)",
    ),
    (
        "/moa",
        "Run one prompt through the default Mixture of Agents preset, then restore your model",
    ),
    (
        "/swarm",
        "Swarm orchestration surface (`status|plan|run|cancel|artifact`) with quorum-compatible controls",
    ),
    ("/swarms", "Alias for /swarm"),
    (
        "/simulate",
        "Simulate tool-policy decisions without executing tools (`status|<tool> [json-params]`)",
    ),
    (
        "/specpatch",
        "Speculative patch executor (`/specpatch <verify_cmd> | <candidate_cmd_1> | ...`)",
    ),
    (
        "/heatmap",
        "Context coverage heatmap for repo files (`/heatmap [repo-path]`)",
    ),
    (
        "/studio",
        "Replay studio (`/studio replay status|verify|diff <export_a.json> <export_b.json>`)",
    ),
    ("/goal", "Alias for /objective"),
    (
        "/ask",
        "Open interactive question picker (`/ask <question> | <option 1> | <option 2> ...`)",
    ),
    ("/question", "Alias for /ask"),
    ("/steer", "Inject non-interrupt steering instruction"),
    ("/btw", "Run an ephemeral side-question"),
    (
        "/plan",
        "Queue planning work or inspect planner queue status (`/plan caps ...`, `/plan depth ...`)",
    ),
    ("/lsp", "Show code-index/LSP context status and controls"),
    (
        "/graph",
        "Show graph-memory, ContextLattice status, and embedding diagnostics",
    ),
    (
        "/qos",
        "Provider QoS router controls (`status|health|autotune [plan|apply]`)",
    ),
    ("/image", "Attach/clear an image hint consumed by next prompt"),
    ("/config", "Show or modify configuration"),
    (
        "/autocompact",
        "Show auto-compaction status (`/autocompact status|now|governance`)",
    ),
    ("/autocompress", "Alias for /autocompact"),
    ("/compress", "Trigger context compression"),
    ("/compact", "Alias for /compress"),
    ("/clear-queue", "Clear queued background jobs"),
    ("/usage", "Show token usage statistics"),
    (
        "/credits",
        "Show Nous credit balance and local usage statistics",
    ),
    (
        "/billing",
        "Show Nous billing/credits summary and local usage statistics",
    ),
    ("/insights", "Show local usage/session insights"),
    ("/stop", "Stop current agent execution"),
    ("/busy", "Busy/processing status compatibility surface"),
    (
        "/kanban",
        "Task board controls (`status|boards|init|use|add|move|claim|block|done|archive-done|dispatch|sync`)",
    ),
    ("/status", "Show session status (model, turns, token count)"),
    ("/agent", "Alias for /status"),
    (
        "/about",
        "Show build/parity/upstream snapshot and enabled Ultra features",
    ),
    ("/ops", "Operator control plane (status + quick controls)"),
    (
        "/telemetry",
        "Live telemetry snapshot (`status|lane`) for runtime health and gate signals",
    ),
    (
        "/runbook",
        "Failure-first remediation runbooks (`list|show <name>`)",
    ),
    (
        "/eval",
        "Run/show live session evaluation harness (`status|run|latest`)",
    ),
    (
        "/autopilot",
        "Adaptive intelligence-performance autopilot (`status|run|recommend|apply|profile|mode|clear`)",
    ),
    (
        "/mission",
        "Mission control board (`status|init|recover|replay|enqueue|trading ...`)",
    ),
    ("/dashboard", "Dashboard control (status|on|off|url)"),
    (
        "/platforms",
        "Show enabled gateway/messaging platform adapters",
    ),
    (
        "/platform",
        "Pause, resume, or list a failing gateway platform (`list|pause|resume`)",
    ),
    ("/gateway", "Alias for /platforms"),
    (
        "/integrations",
        "Integration control plane (`status|auth|providers|gateway|memory|all|repair|snapshot`)",
    ),
    ("/commands", "Show categorized slash command catalog"),
    (
        "/boot",
        "Startup readiness gate (`status|quick|profile`) with pass/warn/fail remediation",
    ),
    (
        "/walkthrough",
        "Guided onboarding walkthrough (`status|start|next|done|reset|insights`)",
    ),
    (
        "/triage",
        "External trigger triage (`status|list|eval|queue|feedback`)",
    ),
    (
        "/subconscious",
        "Background subconscious queue (`status|add|approve|reject|run|profile|clear`)",
    ),
    ("/log", "Show recent runtime log files"),
    ("/debug", "Generate local debug-report guidance"),
    ("/debug-dump", "Write local session diagnostics snapshot"),
    ("/dump-format", "Show concrete transcript snapshot schema"),
    ("/experiment", "Set/clear experiment steering context"),
    ("/feedback", "Record feedback note into local logs"),
    ("/copy", "Copy latest assistant message (if supported)"),
    ("/paste", "Attach clipboard payload (if supported)"),
    ("/gquota", "Show Google quota hint (if configured)"),
    ("/sethome", "Set home channel/session marker"),
    ("/set-home", "Alias for /sethome"),
    ("/restart", "Restart current interactive session"),
    ("/approve", "Approve pending action (gateway mode)"),
    ("/deny", "Deny pending action (gateway mode)"),
    ("/update", "Run update checker and report status"),
    ("/save", "Save current session to disk"),
    ("/load", "Load a saved session"),
    ("/resume", "Resume the most recent or named saved session"),
    (
        "/sessions",
        "Browse saved sessions, or resume one by name (`/sessions [name]`)",
    ),
    ("/session", "Alias for /sessions"),
    ("/switch", "Alias for /sessions"),
    (
        "/background",
        "Run a task in the background (`status|tail <job-id> [N]`)",
    ),
    ("/bg", "Alias for /background"),
    ("/mouse", "Toggle mouse interactions in the TUI"),
    ("/verbose", "Toggle verbose mode"),
    ("/statusbar", "Toggle status bar visibility"),
    ("/footer", "Footer visibility compatibility surface"),
    ("/indicator", "Status indicator compatibility surface"),
    ("/sb", "Alias for /statusbar"),
    ("/yolo", "Toggle auto-approve mode"),
    (
        "/browser",
        "Manage local Chrome CDP bridge (`status|connect [ws/http-url]|disconnect`)",
    ),
    ("/redraw", "Force a local repaint pulse in the TUI"),
    (
        "/reasoning",
        "Reasoning controls (display + effort: status/on/off/full/clamp/set <level>)",
    ),
    (
        "/raw",
        "RTK raw-mode controls + deterministic trace controls (status/on/off/toggle/once/trace with tail/verify/export/path)",
    ),
    (
        "/policy",
        "Runtime policy profiles (`status|list|strict|standard|dev`) + live counters",
    ),
    ("/help", "Show help for available commands"),
    ("/quit", "Quit the application"),
    ("/exit", "Alias for /quit"),
    ("/onboard", "Alias for /walkthrough"),
];

const DEFAULT_SKILL_TAPS: &[&str] = &[
    "https://github.com/NousResearch/hermes-agent::skills",
    "https://github.com/NousResearch/hermes-agent::optional-skills",
    "https://github.com/openai/skills::skills",
    "https://github.com/anthropics/skills::skills",
    "https://github.com/VoltAgent/awesome-agent-skills::skills",
    "https://github.com/mattpocock/skills::skills",
    "https://github.com/github/awesome-copilot::skills",
    "https://github.com/garrytan/gstack::",
    "https://github.com/MiniMax-AI/cli::skill",
];

const GITHUB_API_BASE: &str = "https://api.github.com";
const OFFICIAL_SKILLS_REPO: &str = "nousresearch/hermes-agent";
const HERMES_SKILLS_INDEX_URL: &str =
    "https://hermes-agent.nousresearch.com/docs/api/skills-index.json";
const SKILLS_SH_SEARCH_URL: &str = "https://skills.sh/api/search";
const CLAWHUB_API_BASE: &str = "https://clawhub.ai/api/v1";
const SKILLS_HUB_STATE_DIR: &str = ".hub";
const SKILLS_HUB_LOCK_FILE: &str = "lock.json";
const SKILLS_HUB_AUDIT_FILE: &str = "audit.log";
const SKILLS_HUB_LOCK_VERSION: u32 = 1;
const SENTRUX_MCP_SERVER_NAME: &str = "sentrux";
const SENTRUX_MCP_COMMAND: &str = "sentrux";
const SENTRUX_MCP_ARG: &str = "--mcp";
const UNREAL_MCP_SERVER_NAME: &str = "unreal-engine";
const UNREAL_MCP_URL: &str = "http://127.0.0.1:8000/mcp";
const SKILL_BOOTSTRAP_ALLOWED_EXECUTABLES: &[&str] = &[
    "bash", "sh", "python", "python3", "pip", "pip3", "pipx", "uv", "uvx", "node", "npm", "npx",
    "pnpm", "yarn", "bun", "cargo", "rustup", "go", "make", "cmake", "git", "brew", "apt",
    "apt-get", "dnf", "yum", "pacman", "zypper", "apk",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillTapSpec {
    repo: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSkillSource {
    repo: String,
    branch: String,
    skill_dir: String,
}

#[derive(Debug, Clone)]
enum RegistryInstallSource {
    GitRepo(ResolvedSkillSource),
    LobeRegistry {
        slug: String,
    },
    ClawRegistry {
        slug: String,
        version: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct RegistrySkillRecord {
    identifier: String,
    description: String,
    source: String,
    score: i32,
    install_source: RegistryInstallSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallFallbackSource {
    SkillsSh,
    Tap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillHubInstalledEntry {
    name: String,
    source: String,
    identifier: String,
    trust_level: String,
    scan_verdict: String,
    content_hash: String,
    install_path: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    metadata: serde_json::Value,
    installed_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillsHubLockFile {
    #[serde(default = "default_skills_hub_lock_version")]
    version: u32,
    #[serde(default)]
    installed: Vec<SkillHubInstalledEntry>,
}

impl Default for SkillsHubLockFile {
    fn default() -> Self {
        Self {
            version: SKILLS_HUB_LOCK_VERSION,
            installed: Vec::new(),
        }
    }
}

fn default_skills_hub_lock_version() -> u32 {
    SKILLS_HUB_LOCK_VERSION
}

#[derive(Debug, Clone)]
struct SkillInstallProvenance {
    source: String,
    identifier: String,
    trust_level: String,
    metadata: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
struct SkillBootstrapPlan {
    commands: Vec<String>,
}

#[derive(Debug, Clone)]
struct ParsedBootstrapCommand {
    display: String,
    executable: String,
    args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HermesSkillsIndexResponse {
    #[serde(default)]
    skills: Vec<HermesSkillsIndexEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct HermesSkillsIndexEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    resolved_github_id: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SkillsShSearchResponse {
    #[serde(default)]
    skills: Vec<SkillsShSearchEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillsShSearchEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    #[serde(rename = "skillId")]
    skill_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LobeHubMeta {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct LobeHubAgentResponse {
    #[serde(default)]
    author: String,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    meta: LobeHubMeta,
    #[serde(default)]
    config: LobeHubConfig,
}

#[derive(Debug, Default, Deserialize)]
struct LobeHubConfig {
    #[serde(default)]
    #[serde(rename = "systemRole")]
    system_role: String,
}

#[derive(Debug, Deserialize)]
struct ClawHubSkillDetailResponse {
    #[serde(default)]
    #[serde(rename = "latestVersion")]
    latest_version: ClawHubLatestVersion,
}

#[derive(Debug, Default, Deserialize)]
struct ClawHubLatestVersion {
    #[serde(default)]
    version: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoInfo {
    default_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeResponse {
    tree: Vec<GitHubTreeEntry>,
}

fn parse_skill_tap_spec(raw: &str) -> Option<SkillTapSpec> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (base, override_path) = if let Some((lhs, rhs)) = trimmed.split_once("::") {
        (lhs.trim(), Some(rhs.trim()))
    } else {
        (trimmed, None)
    };

    let (repo, mut path) = if let Some(rest) = base
        .strip_prefix("https://github.com/")
        .or_else(|| base.strip_prefix("http://github.com/"))
    {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let path = if segments.len() >= 5 && segments[2] == "tree" {
            segments[4..].join("/")
        } else {
            "skills".to_string()
        };
        (format!("{}/{}", segments[0], segments[1]), path)
    } else {
        let segments: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let path = if segments.len() > 2 {
            segments[2..].join("/")
        } else {
            "skills".to_string()
        };
        (format!("{}/{}", segments[0], segments[1]), path)
    };

    if let Some(override_path) = override_path {
        path = override_path.to_string();
    }

    Some(SkillTapSpec {
        repo,
        path: path.trim_matches('/').to_string(),
    })
}

fn parse_skill_name_and_version(spec: &str) -> (String, Option<String>) {
    let trimmed = spec.trim();
    if let Some((name, version)) = trimmed.rsplit_once('@') {
        if !name.is_empty() && !version.is_empty() && !name.starts_with("https://") {
            return (name.to_string(), Some(version.to_string()));
        }
    }
    (trimmed.to_string(), None)
}

fn looks_like_github_repo_slug(token: &str) -> bool {
    let parts: Vec<&str> = token.split('/').filter(|s| !s.is_empty()).collect();
    parts.len() == 2
}

fn parse_explicit_github_skill(spec: &str) -> Option<(String, Option<String>, String)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Registry-prefixed identifiers (official/..., skills.sh/..., etc.)
    // must not be treated as direct GitHub owner/repo/path slugs.
    if parse_registry_prefixed_skill(trimmed).is_some() {
        return None;
    }

    if let Some(rest) = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
    {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let repo = format!("{}/{}", segments[0], segments[1]);
        if segments.len() >= 5 && segments[2] == "tree" {
            let branch = segments[3].to_string();
            let path = segments[4..].join("/");
            if path.is_empty() {
                return None;
            }
            return Some((repo, Some(branch), path));
        }
        if segments.len() > 2 {
            let path = segments[2..].join("/");
            if path.is_empty() {
                return None;
            }
            return Some((repo, None, path));
        }
        return None;
    }

    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 3 {
        let repo = format!("{}/{}", segments[0], segments[1]);
        let path = segments[2..].join("/");
        if path.is_empty() {
            return None;
        }
        return Some((repo, None, path));
    }

    None
}

fn sanitize_skill_install_name(source: &str) -> String {
    let raw = source
        .trim()
        .split('/')
        .next_back()
        .unwrap_or(source)
        .trim();
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else if out.ends_with('_') {
            continue;
        } else {
            out.push('_');
        }
    }
    let normalized = out.trim_matches('_').to_string();
    if normalized.is_empty() {
        "skill".to_string()
    } else {
        normalized
    }
}

fn ensure_safe_relative_path(path: &str) -> Result<(), AgentError> {
    if path.is_empty() {
        return Err(AgentError::Config("Empty path in skill bundle.".into()));
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err(AgentError::Config(format!(
            "Unsafe path in skill bundle: {}",
            path
        )));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(AgentError::Config(format!(
                "Unsafe path segment in skill bundle: {}",
                path
            )));
        }
    }
    Ok(())
}

fn parse_registry_prefixed_skill(spec: &str) -> Option<(String, String)> {
    let (prefix, rest) = spec.split_once('/')?;
    let normalized = prefix.trim().to_ascii_lowercase();
    let source = match normalized.as_str() {
        "official" => "official",
        "github" => "github",
        "skills.sh" | "skills-sh" => "skills.sh",
        "lobehub" => "lobehub",
        "clawhub" => "clawhub",
        "claude-marketplace" => "claude-marketplace",
        _ => return None,
    };
    let key = rest.trim();
    if key.is_empty() {
        return None;
    }
    Some((source.to_string(), key.to_string()))
}

fn score_registry_match(entry: &HermesSkillsIndexEntry, query: &str) -> i32 {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return 0;
    }

    let name = entry.name.to_ascii_lowercase();
    let id = entry.identifier.to_ascii_lowercase();
    let desc = entry.description.to_ascii_lowercase();
    let tags = entry
        .tags
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    if id == q || name == q {
        return 1000;
    }
    if id.starts_with(&q) || name.starts_with(&q) {
        return 900;
    }
    if id.contains(&q) || name.contains(&q) {
        return 700;
    }
    if tags.contains(&q) {
        return 550;
    }
    if desc.contains(&q) {
        return 450;
    }
    0
}

fn skill_source_priority(source: &str) -> usize {
    match source.trim().to_ascii_lowercase().as_str() {
        "official" => 0,
        "skills.sh" | "skills-sh" => 1,
        "well-known" => 2,
        "url" => 3,
        "github" => 4,
        "clawhub" => 5,
        "claude-marketplace" => 6,
        "lobehub" => 7,
        _ => 99,
    }
}

fn sort_registry_skill_records(records: &mut [RegistrySkillRecord]) {
    records.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| skill_source_priority(&a.source).cmp(&skill_source_priority(&b.source)))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
}

async fn fetch_hermes_skills_index(
    client: &reqwest::Client,
) -> Result<Vec<HermesSkillsIndexEntry>, AgentError> {
    let resp = client
        .get(HERMES_SKILLS_INDEX_URL)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("Skills index request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "Skills index lookup failed ({}): {}",
            status, body
        )));
    }
    let payload = resp
        .json::<HermesSkillsIndexResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills index response: {}", e)))?;
    Ok(payload.skills)
}

fn resolved_source_from_index(entry: &HermesSkillsIndexEntry) -> Option<RegistryInstallSource> {
    let source = entry.source.to_ascii_lowercase();
    if source == "lobehub" {
        let slug = entry
            .identifier
            .strip_prefix("lobehub/")
            .unwrap_or(entry.identifier.as_str())
            .trim()
            .to_string();
        if slug.is_empty() {
            return None;
        }
        return Some(RegistryInstallSource::LobeRegistry { slug });
    }
    if source == "clawhub" {
        let slug = entry.identifier.trim().to_string();
        if slug.is_empty() {
            return None;
        }
        return Some(RegistryInstallSource::ClawRegistry {
            slug,
            version: None,
        });
    }
    if source == "official" {
        let path = entry.path.trim().trim_matches('/');
        if path.is_empty() {
            return None;
        }
        return Some(RegistryInstallSource::GitRepo(ResolvedSkillSource {
            repo: OFFICIAL_SKILLS_REPO.to_string(),
            branch: "main".to_string(),
            skill_dir: path.to_string(),
        }));
    }

    if let Some(resolved) = entry.resolved_github_id.as_deref() {
        if let Some((repo, _, skill_dir)) = parse_explicit_github_skill(resolved) {
            return Some(RegistryInstallSource::GitRepo(ResolvedSkillSource {
                repo,
                branch: "main".to_string(),
                skill_dir,
            }));
        }
    }

    if !entry.repo.trim().is_empty() {
        let dir = if !entry.path.trim().is_empty() {
            entry.path.trim_matches('/').to_string()
        } else {
            // claude-marketplace entries often point at repo root collections.
            "skills".to_string()
        };
        return Some(RegistryInstallSource::GitRepo(ResolvedSkillSource {
            repo: entry.repo.trim().to_string(),
            branch: "main".to_string(),
            skill_dir: dir,
        }));
    }

    None
}

async fn search_multi_registry(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<RegistrySkillRecord>, AgentError> {
    let entries = fetch_hermes_skills_index(client).await?;
    let mut matches: Vec<RegistrySkillRecord> = Vec::new();
    for entry in entries {
        let score = score_registry_match(&entry, query);
        if score <= 0 {
            continue;
        }
        let Some(install_source) = resolved_source_from_index(&entry) else {
            continue;
        };
        matches.push(RegistrySkillRecord {
            identifier: entry.identifier.clone(),
            description: entry.description.clone(),
            source: entry.source.clone(),
            score,
            install_source,
        });
    }

    sort_registry_skill_records(&mut matches);
    if matches.len() > limit {
        matches.truncate(limit);
    }
    Ok(matches)
}

async fn resolve_skill_via_registry_index(
    client: &reqwest::Client,
    requested: &str,
    source_hint: Option<&str>,
) -> Result<RegistrySkillRecord, AgentError> {
    let entries = fetch_hermes_skills_index(client).await?;
    let requested_l = requested.trim().to_ascii_lowercase();
    let source_hint = source_hint.map(|s| s.to_ascii_lowercase());

    let mut exact: Vec<RegistrySkillRecord> = Vec::new();
    let mut fuzzy: Vec<RegistrySkillRecord> = Vec::new();
    for entry in entries {
        if let Some(ref hint) = source_hint {
            if entry.source.to_ascii_lowercase() != *hint {
                continue;
            }
        }
        let Some(install_source) = resolved_source_from_index(&entry) else {
            continue;
        };
        let source_l = entry.source.to_ascii_lowercase();
        let identifier_l = entry.identifier.to_ascii_lowercase();
        let name_l = entry.name.to_ascii_lowercase();
        let source_scoped = format!("{}/{}", source_l, name_l);
        let source_scoped_id = format!("{}/{}", source_l, identifier_l);
        let rec = RegistrySkillRecord {
            identifier: entry.identifier.clone(),
            description: entry.description.clone(),
            source: entry.source.clone(),
            score: score_registry_match(&entry, requested),
            install_source,
        };
        if requested_l == identifier_l
            || requested_l == name_l
            || requested_l == source_scoped
            || requested_l == source_scoped_id
        {
            exact.push(rec);
        } else if identifier_l.contains(&requested_l) || name_l.contains(&requested_l) {
            fuzzy.push(rec);
        }
    }

    sort_registry_skill_records(&mut exact);
    sort_registry_skill_records(&mut fuzzy);

    if let Some(first) = exact.into_iter().next() {
        return Ok(first);
    }
    if let Some(first) = fuzzy.into_iter().next() {
        return Ok(first);
    }
    Err(AgentError::Config(format!(
        "Skill '{}' was not found in multi-registry index.",
        requested
    )))
}

fn build_lobehub_skill_markdown(payload: &LobeHubAgentResponse, slug: &str) -> String {
    let title = if payload.meta.title.trim().is_empty() {
        slug.to_string()
    } else {
        payload.meta.title.trim().to_string()
    };
    let description = if payload.meta.description.trim().is_empty() {
        payload.summary.trim().to_string()
    } else {
        payload.meta.description.trim().to_string()
    };
    let role = payload.config.system_role.trim();

    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("name: {}\n", slug));
    if !description.is_empty() {
        md.push_str(&format!(
            "description: {}\n",
            description.replace('\n', " ")
        ));
    }
    md.push_str("category: lobehub\n");
    md.push_str("---\n\n");
    md.push_str(&format!("# {}\n\n", title));
    if !description.is_empty() {
        md.push_str(&format!("{}\n\n", description));
    }
    md.push_str("## Source\n");
    md.push_str(&format!("- Registry: lobehub\n- Identifier: {}\n", slug));
    if !payload.author.trim().is_empty() {
        md.push_str(&format!("- Author: {}\n", payload.author.trim()));
    }
    if !payload.homepage.trim().is_empty() {
        md.push_str(&format!("- Homepage: {}\n", payload.homepage.trim()));
    }
    md.push_str("\n## Instructions\n");
    if role.is_empty() {
        md.push_str("No system role provided by source registry.\n");
    } else {
        md.push_str(role);
        md.push('\n');
    }
    md
}

fn default_trust_level_for_source(source: &str) -> &'static str {
    match source {
        "official" => "builtin",
        "skills.sh" | "hermes-index" | "claude-marketplace" | "github" | "tap" => "trusted",
        "lobehub" | "clawhub" => "community",
        _ => "community",
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn skills_hub_state_dir(skills_dir: &Path) -> PathBuf {
    skills_dir.join(SKILLS_HUB_STATE_DIR)
}

fn skills_hub_lock_path(skills_dir: &Path) -> PathBuf {
    skills_hub_state_dir(skills_dir).join(SKILLS_HUB_LOCK_FILE)
}

fn skills_hub_audit_path(skills_dir: &Path) -> PathBuf {
    skills_hub_state_dir(skills_dir).join(SKILLS_HUB_AUDIT_FILE)
}

fn read_skills_hub_lock(skills_dir: &Path) -> SkillsHubLockFile {
    let path = skills_hub_lock_path(skills_dir);
    let Ok(raw) = std::fs::read_to_string(path) else {
        return SkillsHubLockFile::default();
    };
    serde_json::from_str::<SkillsHubLockFile>(&raw).unwrap_or_default()
}

fn write_skills_hub_lock(skills_dir: &Path, lock: &SkillsHubLockFile) -> Result<(), AgentError> {
    let state_dir = skills_hub_state_dir(skills_dir);
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create skills hub state dir '{}': {}",
            state_dir.display(),
            e
        ))
    })?;
    let path = skills_hub_lock_path(skills_dir);
    let body = serde_json::to_string_pretty(lock)
        .map_err(|e| AgentError::Config(format!("Failed to serialize skills hub lock: {}", e)))?;
    std::fs::write(&path, body).map_err(|e| {
        AgentError::Io(format!(
            "Failed to write skills hub lock '{}': {}",
            path.display(),
            e
        ))
    })
}

fn append_skills_hub_audit(
    skills_dir: &Path,
    action: &str,
    entry: &SkillHubInstalledEntry,
) -> Result<(), AgentError> {
    let state_dir = skills_hub_state_dir(skills_dir);
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create skills hub state dir '{}': {}",
            state_dir.display(),
            e
        ))
    })?;
    let path = skills_hub_audit_path(skills_dir);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| {
            AgentError::Io(format!(
                "Failed to open skills hub audit log '{}': {}",
                path.display(),
                e
            ))
        })?;
    let line = serde_json::json!({
        "timestamp": now_rfc3339(),
        "action": action,
        "name": entry.name,
        "source": entry.source,
        "identifier": entry.identifier,
        "trust_level": entry.trust_level,
        "scan_verdict": entry.scan_verdict,
        "content_hash": entry.content_hash,
    });
    use std::io::Write as _;
    writeln!(file, "{}", line)
        .map_err(|e| AgentError::Io(format!("Failed to append skills hub audit log: {}", e)))
}

fn hash_skill_bundle(files: &[(String, Bytes)]) -> String {
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel_path, bytes) in sorted {
        h.update(rel_path.as_bytes());
        h.update([0]);
        h.update(bytes.as_ref());
        h.update([0xFF]);
    }
    format!("sha256:{:x}", h.finalize())
}

fn collect_skill_files_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), AgentError> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| AgentError::Io(format!("Failed to read dir '{}': {}", dir.display(), e)))?
    {
        let entry = entry.map_err(|e| {
            AgentError::Io(format!(
                "Failed to read dir entry '{}': {}",
                dir.display(),
                e
            ))
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| {
            AgentError::Io(format!(
                "Failed to get file type for '{}': {}",
                path.display(),
                e
            ))
        })?;
        if file_type.is_dir() {
            collect_skill_files_recursive(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| AgentError::Io(format!("Failed to compute relative path: {}", e)))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path)
            .map_err(|e| AgentError::Io(format!("Failed to read '{}': {}", path.display(), e)))?;
        out.push((rel, bytes));
    }
    Ok(())
}

fn hash_installed_skill_dir(skill_dir: &Path) -> Result<String, AgentError> {
    if !skill_dir.exists() {
        return Err(AgentError::Config(format!(
            "Installed skill path does not exist: {}",
            skill_dir.display()
        )));
    }
    let mut files = Vec::new();
    collect_skill_files_recursive(skill_dir, skill_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel_path, bytes) in files {
        h.update(rel_path.as_bytes());
        h.update([0]);
        h.update(&bytes);
        h.update([0xFF]);
    }
    Ok(format!("sha256:{:x}", h.finalize()))
}

fn record_skill_install_in_hub_lock(
    skills_dir: &Path,
    installed_name: &str,
    install_path: &Path,
    files: &[(String, Bytes)],
    provenance: &SkillInstallProvenance,
) -> Result<(), AgentError> {
    let mut lock = read_skills_hub_lock(skills_dir);
    let now = now_rfc3339();
    let install_path_rel = install_path
        .strip_prefix(skills_dir)
        .unwrap_or(install_path)
        .to_string_lossy()
        .replace('\\', "/");
    let content_hash = hash_installed_skill_dir(install_path)?;
    let files_rel: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let entry = SkillHubInstalledEntry {
        name: installed_name.to_string(),
        source: provenance.source.clone(),
        identifier: provenance.identifier.clone(),
        trust_level: provenance.trust_level.clone(),
        scan_verdict: "clean".to_string(),
        content_hash,
        install_path: install_path_rel,
        files: files_rel,
        metadata: provenance.metadata.clone(),
        installed_at: now.clone(),
        updated_at: now,
    };
    lock.installed.retain(|item| item.name != installed_name);
    lock.installed.push(entry.clone());
    lock.installed.sort_by(|a, b| a.name.cmp(&b.name));
    write_skills_hub_lock(skills_dir, &lock)?;
    append_skills_hub_audit(skills_dir, "INSTALL", &entry)?;
    Ok(())
}

fn record_skill_uninstall_in_hub_lock(
    skills_dir: &Path,
    skill_name: &str,
) -> Result<Option<SkillHubInstalledEntry>, AgentError> {
    let mut lock = read_skills_hub_lock(skills_dir);
    let mut removed: Option<SkillHubInstalledEntry> = None;
    lock.installed.retain(|entry| {
        if entry.name == skill_name {
            removed = Some(entry.clone());
            false
        } else {
            true
        }
    });
    write_skills_hub_lock(skills_dir, &lock)?;
    if let Some(ref removed_entry) = removed {
        append_skills_hub_audit(skills_dir, "UNINSTALL", removed_entry)?;
    }
    Ok(removed)
}

fn skill_guard_scan_bundle(files: &[(String, Bytes)]) -> Result<(), AgentError> {
    let guard = hermes_skills::SkillGuard::default();
    for (rel_path, bytes) in files {
        // Skip binary files to avoid false positives from compressed payloads.
        let Ok(text) = std::str::from_utf8(bytes.as_ref()) else {
            continue;
        };
        let probe = hermes_core::types::Skill {
            name: rel_path.clone(),
            content: text.to_string(),
            category: Some("external".to_string()),
            description: None,
        };
        guard.scan_security_only(&probe).map_err(|e| {
            AgentError::Config(format!(
                "Security scan failed for skill bundle file '{}': {}",
                rel_path, e
            ))
        })?;
    }
    Ok(())
}

fn github_request(client: &reqwest::Client, url: &str, accept: &str) -> reqwest::RequestBuilder {
    let mut req = client
        .get(url)
        .header("Accept", accept)
        .header("User-Agent", "hermes-agent-ultra");
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .map(|v| v.trim().to_string())
    {
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
    }
    req
}

async fn github_default_branch(client: &reqwest::Client, repo: &str) -> Result<String, AgentError> {
    let url = format!("{}/repos/{}", GITHUB_API_BASE, repo);
    let resp = github_request(client, &url, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("GitHub request failed for {}: {}", repo, e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "GitHub repo lookup failed for {} ({}): {}",
            repo, status, body
        )));
    }
    let payload = resp
        .json::<GitHubRepoInfo>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid GitHub repo response: {}", e)))?;
    Ok(payload.default_branch)
}

async fn github_repo_tree(
    client: &reqwest::Client,
    repo: &str,
    branch: &str,
) -> Result<Vec<GitHubTreeEntry>, AgentError> {
    let encoded_branch = urlencoding::encode(branch);
    let url = format!(
        "{}/repos/{}/git/trees/{}?recursive=1",
        GITHUB_API_BASE, repo, encoded_branch
    );
    let resp = github_request(client, &url, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("GitHub tree request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "GitHub tree lookup failed for {repo}@{branch} ({status}): {body}"
        )));
    }
    let payload = resp
        .json::<GitHubTreeResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid GitHub tree response: {}", e)))?;
    Ok(payload.tree)
}

include!("command_catalog/skill_resolution.rs");
