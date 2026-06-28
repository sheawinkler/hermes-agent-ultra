/// Return auto-completion suggestions for a partial slash command.
pub fn autocomplete(partial: &str) -> Vec<&'static str> {
    hermes_cli_ui::autocomplete(partial, SLASH_COMMANDS)
}

/// Return contextual auto-completion suggestions for slash commands.
///
/// Unlike [`autocomplete`], this understands command argument position and can
/// suggest nested values like `/swarm run <passes> <mode>`.
pub fn autocomplete_contextual(partial: &str) -> Vec<String> {
    autocomplete_contextual_with_runtime(partial, None)
}

pub fn autocomplete_contextual_for_app(partial: &str, app: &App) -> Vec<String> {
    autocomplete_contextual_with_runtime(
        partial,
        Some(CompletionRuntime {
            config: app.config.as_ref(),
            tool_registry: app.tool_registry.as_ref(),
        }),
    )
}

struct CompletionRuntime<'a> {
    config: &'a GatewayConfig,
    tool_registry: &'a hermes_tools::ToolRegistry,
}

fn autocomplete_contextual_with_runtime(
    partial: &str,
    runtime: Option<CompletionRuntime<'_>>,
) -> Vec<String> {
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

    // First token only: preserve current fuzzy top-level behavior.
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

    let candidates = command_argument_candidates(&cmd, &args, arg_position, runtime.as_ref());

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

fn command_argument_candidates(
    cmd: &str,
    args: &[&str],
    arg_position: usize,
    runtime: Option<&CompletionRuntime<'_>>,
) -> Vec<String> {
    match (cmd, arg_position) {
        ("/personality", 0) => personality_completion_candidates(),
        ("/handoff", 0) => handoff_completion_candidates(runtime),
        ("/tools", 0) => ["list", "trust", "enable", "disable"]
            .iter()
            .map(|v| (*v).to_string())
            .collect(),
        ("/tools", 1) => args
            .first()
            .map(|sub| tool_completion_candidates(runtime, sub))
            .unwrap_or_default(),
        _ if arg_position == 0 => command_subcommand_candidates(cmd),
        _ => command_nested_candidates(cmd, args[0], arg_position),
    }
}

fn personality_completion_candidates() -> Vec<String> {
    let mut out = vec!["list".to_string(), "none".to_string()];
    out.extend(
        hermes_agent::builtin_personality_names()
            .iter()
            .map(|v| (*v).to_string()),
    );
    out.sort();
    out.dedup();
    out
}

fn handoff_completion_candidates(runtime: Option<&CompletionRuntime<'_>>) -> Vec<String> {
    let Some(runtime) = runtime else {
        return Vec::new();
    };
    let mut out: Vec<String> = runtime
        .config
        .platforms
        .iter()
        .filter(|(_, platform)| platform.enabled)
        .map(|(name, _)| name.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn tool_completion_candidates(
    runtime: Option<&CompletionRuntime<'_>>,
    subcommand: &str,
) -> Vec<String> {
    let Some(runtime) = runtime else {
        return Vec::new();
    };
    let action = subcommand.trim().to_ascii_lowercase();
    if action != "enable" && action != "disable" {
        return Vec::new();
    }

    let disabled: HashSet<&str> = runtime
        .config
        .tools_config
        .disabled
        .iter()
        .map(String::as_str)
        .collect();

    let mut out: Vec<String> = runtime
        .tool_registry
        .list_tools()
        .into_iter()
        .filter_map(|tool| {
            let active = !disabled.contains(tool.name.as_str());
            match (action.as_str(), active) {
                ("enable", false) | ("disable", true) => Some(tool.name),
                _ => None,
            }
        })
        .collect();
    out.sort();
    out.dedup();
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
        ("/objective", "wait", 1) => ["--session", "--seconds", "for"]
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
            "wait",
            "unwait",
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

/// Return the help text for a specific slash command.
pub fn help_for(cmd: &str) -> Option<&'static str> {
    hermes_cli_ui::help_for(cmd, SLASH_COMMANDS)
}

fn canonical_command(cmd: &str) -> &str {
    hermes_cli_ui::canonical_command(cmd)
}
