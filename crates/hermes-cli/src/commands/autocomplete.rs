//! Slash-command auto-completion and canonical name resolution.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use hermes_config::QuickCommandConfig;

/// All supported slash commands and their descriptions.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/new", "Start a new session"),
    (
        "/reset",
        "Start a new session (alias of /new; fresh session ID + history)",
    ),
    (
        "/clear",
        "Clear screen/session state and start a fresh session",
    ),
    ("/retry", "Retry the last user message"),
    ("/undo", "Undo the last exchange"),
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
    ("/fast", "Toggle fast-mode hints"),
    ("/skin", "Show available skin/theme options"),
    ("/skins", "Alias for /skin"),
    ("/voice", "Show voice mode status"),
    (
        "/pet",
        "Animated companion controls (`status|on|off|toggle|list|set|mood|dock|speed`)",
    ),
    ("/skills", "List available skills"),
    ("/skill", "Alias for /skills"),
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
    ("/mcp", "List configured MCP servers"),
    ("/reload", "Reload runtime env/config values"),
    ("/reload-skills", "Refresh installed skill index/registry"),
    ("/reload_skills", "Alias for /reload-skills"),
    ("/reload-mcp", "Reload MCP server metadata"),
    ("/reload_mcp", "Alias for /reload-mcp"),
    ("/cron", "Show cron scheduler status"),
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
        "Set/show objective contract + profile/policies (`status|verify|plan|constraints|counterfactual|profile|context|simulator|ensemble|ledger|dag|eval|clear`)",
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
    (
        "/image",
        "Attach/clear an image hint consumed by next prompt",
    ),
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
        "/plan-mode",
        "Plan-then-execute (`/plan-mode <task>` or on|approve|reject|edit)",
    ),
    (
        "/browser",
        "Manage local Chrome CDP bridge (`status|connect [ws/http-url]|disconnect`)",
    ),
    ("/redraw", "Force a local repaint pulse in the TUI"),
    (
        "/reasoning",
        "Reasoning controls (display + effort: status/on/off/set <low|medium|high|xhigh>)",
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
    (
        "/acp_server",
        "ACP server (auto-start if not running; or start|stop|status|restart|connections)",
    ),
    ("/quit", "Quit the application"),
    ("/exit", "Alias for /quit"),
    ("/onboard", "Alias for /walkthrough"),
];

struct CommandTrieNode {
    children: HashMap<char, CommandTrieNode>,
    terminal: Option<&'static str>,
}

struct CommandTrie {
    root: CommandTrieNode,
}

impl CommandTrieNode {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            terminal: None,
        }
    }
}

impl CommandTrie {
    fn from_commands(commands: &'static [(&'static str, &str)]) -> Self {
        let mut root = CommandTrieNode::new();
        let mut seen = HashSet::new();
        for (cmd, _) in commands {
            if !seen.insert(*cmd) {
                continue;
            }
            let mut node = &mut root;
            for ch in cmd.to_ascii_lowercase().chars() {
                node = node.children.entry(ch).or_insert_with(CommandTrieNode::new);
            }
            node.terminal = Some(cmd);
        }
        Self { root }
    }

    fn prefix_search(&self, prefix: &str) -> Vec<&'static str> {
        let mut node = &self.root;
        for ch in prefix.chars() {
            let Some(next) = node.children.get(&ch) else {
                return Vec::new();
            };
            node = next;
        }
        let mut out = Vec::new();
        Self::collect_terminals(node, &mut out);
        out.sort_unstable();
        out
    }

    fn collect_terminals(node: &CommandTrieNode, out: &mut Vec<&'static str>) {
        if let Some(cmd) = node.terminal {
            out.push(cmd);
        }
        let mut keys: Vec<char> = node.children.keys().copied().collect();
        keys.sort_unstable();
        for key in keys {
            if let Some(child) = node.children.get(&key) {
                Self::collect_terminals(child, out);
            }
        }
    }
}

static COMMAND_TRIE: LazyLock<CommandTrie> = LazyLock::new(|| CommandTrie::from_commands(SLASH_COMMANDS));

/// Return auto-completion suggestions for a partial slash command.
pub fn autocomplete(partial: &str) -> Vec<&'static str> {
    let query = partial.trim().to_ascii_lowercase();
    if query.is_empty() || query == "/" {
        let mut seen = HashSet::new();
        let mut out: Vec<&'static str> = Vec::new();
        for (cmd, _) in SLASH_COMMANDS {
            if seen.insert(*cmd) {
                out.push(cmd);
            }
        }
        out.sort_unstable();
        return out;
    }

    let prefix_matches = COMMAND_TRIE.prefix_search(&query);
    if !prefix_matches.is_empty() {
        let mut seen = HashSet::new();
        let mut out: Vec<&'static str> = Vec::new();
        for cmd in prefix_matches {
            if seen.insert(cmd) {
                out.push(cmd);
            }
        }
        out.sort_by(|a, b| {
            a.len()
                .cmp(&b.len())
                .then_with(|| a.cmp(b))
        });
        return out;
    }

    let mut seen = HashSet::new();
    let mut ranked: Vec<(&'static str, i32)> = Vec::new();
    for (cmd, desc) in SLASH_COMMANDS {
        if !seen.insert(*cmd) {
            continue;
        }
        if let Some(score) = command_match_score(&query, cmd, desc) {
            ranked.push((cmd, score));
        }
    }
    ranked.sort_by(|(a_cmd, a_score), (b_cmd, b_score)| {
        b_score.cmp(a_score).then_with(|| a_cmd.cmp(b_cmd))
    });
    ranked.into_iter().map(|(cmd, _)| cmd).collect()
}

/// Return contextual auto-completion suggestions for slash commands.
///
/// Unlike [`autocomplete`], this understands command argument position and can
/// suggest nested values like `/swarm run <passes> <mode>`.
pub fn autocomplete_contextual(partial: &str) -> Vec<String> {
    let trimmed_start = partial.trim_start();
    if !trimmed_start.starts_with('/') {
        return Vec::new();
    }
    let trailing_space = trimmed_start
        .chars()
        .last()
        .is_some_and(char::is_whitespace);
    let tokens: Vec<&str> = trimmed_start.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    if tokens.len() == 1 && !trailing_space {
        return autocomplete(trimmed_start)
            .into_iter()
            .map(ToString::to_string)
            .collect();
    }

    let Some(cmd) = resolve_completion_command(tokens[0]) else {
        return autocomplete(tokens[0])
            .into_iter()
            .map(ToString::to_string)
            .collect();
    };

    let args = if tokens.len() > 1 {
        tokens[1..].to_vec()
    } else {
        Vec::new()
    };

    let (arg_position, fragment) = if args.is_empty() {
        (0usize, "")
    } else if trailing_space {
        (args.len(), "")
    } else {
        (args.len() - 1, args[args.len() - 1])
    };

    let candidates = if arg_position == 0 {
        command_subcommand_candidates(&cmd)
    } else {
        command_nested_candidates(&cmd, args[0], arg_position)
    };

    if candidates.is_empty() {
        return Vec::new();
    }

    let fragment_lc = fragment.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for candidate in candidates {
        if !fragment_lc.is_empty() && !candidate.to_ascii_lowercase().starts_with(&fragment_lc) {
            continue;
        }
        let mut parts: Vec<String> = Vec::with_capacity(1 + arg_position + 1);
        parts.push(cmd.clone());
        for i in 0..arg_position {
            if i < args.len() {
                parts.push(args[i].to_string());
            }
        }
        parts.push(candidate.to_string());
        let mut suggestion = parts.join(" ");
        if trailing_space {
            suggestion.push(' ');
        }
        if seen.insert(suggestion.clone()) {
            out.push(suggestion);
        }
    }
    out
}

fn resolve_completion_command(raw: &str) -> Option<String> {
    let canonical = canonical_command(raw);
    if SLASH_COMMANDS.iter().any(|(name, _)| *name == canonical) {
        return Some(canonical.to_string());
    }
    let exact = autocomplete(raw);
    if exact.len() == 1 {
        return exact
            .first()
            .copied()
            .map(canonical_command)
            .map(ToString::to_string);
    }
    None
}

fn command_subcommand_candidates(cmd: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for value in command_subcommand_overrides(cmd) {
        if seen.insert(value.to_string()) {
            out.push(value.to_string());
        }
    }
    for value in inferred_subcommands_from_description(cmd) {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn command_nested_candidates(cmd: &str, subcommand: &str, arg_position: usize) -> Vec<String> {
    let sub = subcommand.to_ascii_lowercase();
    match (cmd, sub.as_str(), arg_position) {
        ("/swarm", "plan", 1) => ["concurrent", "sequential", "graph"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/swarm", "run", 1) => ["1", "2", "4", "8", "16", "32", "64"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/swarm", "run", 2) => ["concurrent", "sequential", "graph"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/swarm", "voters", 1) => ["2", "3", "4", "5", "6", "7", "8"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/quorum", "voters", 1) => ["2", "3", "4", "5", "6", "7", "8"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "lifecycle", 1) => [
            "status",
            "active",
            "pause",
            "resume",
            "budget-limited",
            "achieved",
            "unmet",
        ]
        .iter()
        .map(|v| (*v).to_string())
        .collect(),
        ("/objective", "behavior", 1) => [
            "status",
            "list",
            "balanced",
            "strict",
            "autonomous",
            "mission",
            "minimal",
            "sigma",
        ]
        .iter()
        .map(|v| (*v).to_string())
        .collect(),
        ("/objective", "profile", 1) => ["status", "list", "general", "me", "set"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "context", 1) => ["status", "list", "max", "balanced", "fast"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "simulator", 1) => ["status", "balanced", "strict", "aggressive"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "ensemble", 1) => ["status", "committee", "single", "debate"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "ledger", 1) => ["status", "tail", "clear"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "dag", 1) => ["status", "rebuild", "clear"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/objective", "eval", 1) => ["status", "tail"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/model", "why-not", 1) => [
            "--cap",
            "--min-context",
            "--max-input-cost",
            "--max-output-cost",
            "--budget",
        ]
        .iter()
        .map(|v| (*v).to_string())
        .collect(),
        _ => Vec::new(),
    }
}

fn command_subcommand_overrides(cmd: &str) -> &'static [&'static str] {
    match cmd {
        "/auth" => &["status", "verify", "refresh"],
        "/context" => &["status", "breakdown", "compress"],
        "/pet" => &[
            "status", "on", "off", "toggle", "list", "set", "mood", "dock", "speed",
        ],
        "/agents" => &["status", "pause", "resume", "doctor"],
        "/objective" => &[
            "status",
            "verify",
            "plan",
            "constraints",
            "counterfactual",
            "profile",
            "context",
            "simulator",
            "ensemble",
            "ledger",
            "dag",
            "eval",
            "clear",
            "lifecycle",
            "behavior",
        ],
        "/quorum" => &["status", "on", "off", "voters", "models", "run"],
        "/swarm" => &[
            "status", "plan", "run", "cancel", "artifact", "on", "off", "voters", "models",
        ],
        "/simulate" => &["status"],
        "/timetravel" => &["list", "latest", "goto", "undo", "branch"],
        "/autocompact" => &["status", "now", "governance"],
        "/qos" => &["status", "health", "autotune"],
        "/claims" => &["status", "on", "off"],
        "/curator" => &[
            "status",
            "run",
            "pause",
            "resume",
            "pin",
            "unpin",
            "restore",
            "list-archived",
        ],
        _ => &[],
    }
}

fn inferred_subcommands_from_description(cmd: &str) -> Vec<String> {
    let Some((_, desc)) = SLASH_COMMANDS.iter().find(|(name, _)| *name == cmd) else {
        return Vec::new();
    };
    let mut segments: Vec<String> = Vec::new();
    let mut in_tick = false;
    let mut buf = String::new();
    for ch in desc.chars() {
        if ch == '`' {
            if in_tick && !buf.trim().is_empty() {
                segments.push(buf.clone());
            }
            buf.clear();
            in_tick = !in_tick;
            continue;
        }
        if in_tick {
            buf.push(ch);
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for seg in segments {
        for raw in seg.split('|') {
            let cleaned = raw
                .trim()
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .trim_start_matches('/');
            if cleaned.is_empty() {
                continue;
            }
            let lc = cleaned.to_ascii_lowercase();
            if lc == cmd.trim_start_matches('/') {
                continue;
            }
            if !lc
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                continue;
            }
            if seen.insert(lc.clone()) {
                out.push(lc);
            }
        }
    }
    out
}

fn command_match_score(query: &str, cmd: &str, desc: &str) -> Option<i32> {
    if query.is_empty() || query == "/" {
        return Some(10);
    }
    let cmd_l = cmd.to_ascii_lowercase();
    let desc_l = desc.to_ascii_lowercase();
    if cmd_l == query {
        return Some(1200);
    }
    if cmd_l.starts_with(query) {
        return Some(1000 - (cmd_l.len().saturating_sub(query.len()) as i32));
    }
    if cmd_l.contains(query) {
        return Some(850 - (cmd_l.len().saturating_sub(query.len()) as i32));
    }
    if let Some(pos) = desc_l.find(query.trim_start_matches('/')) {
        return Some(700 - pos as i32);
    }
    let subseq = subsequence_score(query.trim_start_matches('/'), cmd_l.trim_start_matches('/'));
    if subseq > 0 {
        return Some(500 + subseq);
    }
    None
}

fn subsequence_score(needle: &str, haystack: &str) -> i32 {
    if needle.is_empty() || haystack.is_empty() {
        return 0;
    }
    let mut score = 0i32;
    let mut idx = 0usize;
    let chars: Vec<char> = haystack.chars().collect();
    for ch in needle.chars() {
        let mut found = false;
        while idx < chars.len() {
            if chars[idx] == ch {
                score += 2;
                if idx > 0 && chars[idx - 1] == '-' {
                    score += 1;
                }
                idx += 1;
                found = true;
                break;
            }
            idx += 1;
        }
        if !found {
            return 0;
        }
    }
    score
}

/// Return the help text for a specific slash command.
pub fn help_for(cmd: &str) -> Option<&'static str> {
    SLASH_COMMANDS
        .iter()
        .find(|(name, _)| *name == cmd)
        .map(|(_, desc)| *desc)
}

pub(crate) fn canonical_command(cmd: &str) -> &str {
    match cmd {
        "/clear" => "/new",
        "/reset" => "/new",
        "/compact" => "/compress",
        "/skill" => "/skills",
        "/agent" => "/status",
        "/tasks" => "/kanban",
        "/busy" => "/status",
        "/topic" => "/title",
        "/scheduler" => "/background",
        "/gateway" => "/platforms",
        "/onboard" => "/walkthrough",
        "/reload-skills" => "/reload",
        "/reload_skills" => "/reload",
        "/reload_mcp" => "/reload-mcp",
        "/fork" => "/branch",
        "/tt" => "/timetravel",
        "/snap" => "/snapshot",
        "/set-home" => "/sethome",
        "/footer" => "/statusbar",
        "/indicator" => "/statusbar",
        "/q" => "/queue",
        "/bg" => "/background",
        "/goal" => "/objective",
        "/swarms" => "/swarm",
        "/question" => "/ask",
        "/autocompress" => "/autocompact",
        "/skins" => "/skin",
        "/summary" => "/recap",
        "/whoami" => "/profile",
        "/sb" => "/statusbar",
        "/pilot" => "/autopilot",
        "/rb" => "/runbook",
        "/debug" => "/debug-dump",
        "/exit" => "/quit",
        other => other,
    }
}

fn quick_command_key(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace('-', "_")
}

pub(crate) fn expand_quick_alias_command(
    quick_commands: &BTreeMap<String, QuickCommandConfig>,
    cmd: &str,
    args: &[&str],
) -> Result<(String, Vec<String>), String> {
    let mut current_cmd = cmd.to_string();
    let mut current_args: Vec<String> = args.iter().map(|part| (*part).to_string()).collect();
    loop {
        let key = quick_command_key(&current_cmd);
        let Some(quick) = quick_commands.get(&key) else {
            return Ok((current_cmd, current_args));
        };
        match quick.kind.trim().to_ascii_lowercase().as_str() {
            "alias" => {
                let Some(target) = quick.target.as_deref().filter(|v| !v.trim().is_empty()) else {
                    return Err(format!("Quick command `{key}` has no target defined."));
                };
                let target = target.trim();
                let (target_cmd, embedded_args) = match target.find(char::is_whitespace) {
                    Some(idx) => (&target[..idx], target[idx..].trim()),
                    None => (target, ""),
                };
                let mut merged = Vec::new();
                if !embedded_args.is_empty() {
                    merged.extend(
                        embedded_args
                            .split_whitespace()
                            .map(|part| part.to_string()),
                    );
                }
                merged.extend(current_args);
                current_cmd = target_cmd.to_string();
                current_args = merged;
            }
            other => {
                return Err(format!(
                    "Quick command `{key}` has unsupported kind `{other}`."
                ));
            }
        }
    }
}
