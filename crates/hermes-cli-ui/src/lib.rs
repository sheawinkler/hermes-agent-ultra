use std::collections::HashSet;
use std::fmt::Write;

#[derive(Debug, Clone, Copy)]
struct CommandCatalogSection {
    title: &'static str,
    hint: &'static str,
    commands: &'static [&'static str],
}

const COMMAND_CATALOG_SECTIONS: &[CommandCatalogSection] = &[
    CommandCatalogSection {
        title: "Core Session",
        hint: "Session lifecycle, snapshots, rollback, and queue controls",
        commands: &[
            "/new",
            "/reset",
            "/retry",
            "/undo",
            "/rewind",
            "/history",
            "/recap",
            "/context",
            "/title",
            "/branch",
            "/timetravel",
            "/snapshot",
            "/rollback",
            "/queue",
            "/background",
            "/save",
            "/load",
            "/resume",
            "/sessions",
            "/session",
            "/switch",
        ],
    },
    CommandCatalogSection {
        title: "Model/Auth",
        hint: "Provider, model, auth, and reasoning controls",
        commands: &[
            "/model",
            "/provider",
            "/auth",
            "/reasoning",
            "/gquota",
            "/qos",
            "/boot",
            "/walkthrough",
            "/version",
        ],
    },
    CommandCatalogSection {
        title: "Objective/Planning",
        hint: "Mission steering, objectives, planning, and simulation",
        commands: &[
            "/objective",
            "/goal",
            "/subgoal",
            "/plan",
            "/ask",
            "/steer",
            "/btw",
            "/simulate",
            "/specpatch",
            "/quorum",
            "/mission",
            "/autopilot",
            "/triage",
            "/subconscious",
        ],
    },
    CommandCatalogSection {
        title: "Tools/Skills/Integrations",
        hint: "Skills, tools, MCP, gateway adapters, and integration health",
        commands: &[
            "/skills",
            "/tools",
            "/toolcards",
            "/toolsets",
            "/plugins",
            "/memory",
            "/mcp",
            "/platforms",
            "/integrations",
            "/reload",
            "/reload-mcp",
            "/runbook",
            "/ops",
            "/telemetry",
            "/dashboard",
        ],
    },
    CommandCatalogSection {
        title: "UX/Views",
        hint: "TUI surface controls and visibility toggles",
        commands: &[
            "/skin",
            "/voice",
            "/pet",
            "/image",
            "/mouse",
            "/verbose",
            "/statusbar",
            "/raw",
            "/redraw",
            "/copy",
            "/paste",
            "/commands",
            "/help",
            "/quit",
        ],
    },
];

pub fn autocomplete(
    partial: &str,
    commands: &'static [(&'static str, &'static str)],
) -> Vec<&'static str> {
    let mut seen = HashSet::new();
    let mut ranked: Vec<(&'static str, i32)> = Vec::new();
    let query = partial.trim().to_ascii_lowercase();
    for (cmd, desc) in commands {
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

pub fn help_for(
    cmd: &str,
    commands: &'static [(&'static str, &'static str)],
) -> Option<&'static str> {
    commands
        .iter()
        .find(|(name, _)| *name == cmd)
        .map(|(_, desc)| *desc)
}

pub fn canonical_command(cmd: &str) -> &str {
    match cmd {
        "/clear" => "/new",
        "/compact" => "/compress",
        "/skill" => "/skills",
        "/codex_runtime" => "/codex-runtime",
        "/curator" => "/skills",
        "/agent" => "/status",
        "/tasks" => "/kanban",
        "/busy" => "/status",
        "/topic" => "/title",
        "/scheduler" => "/background",
        "/gateway" => "/platforms",
        "/onboard" => "/walkthrough",
        "/reload-skills" | "/reload_skills" => "/reload-skills",
        "/reload_mcp" => "/reload-mcp",
        "/fork" => "/branch",
        "/tt" => "/timetravel",
        "/snap" => "/snapshot",
        "/set-home" => "/sethome",
        "/footer" => "/statusbar",
        "/indicator" => "/statusbar",
        "/q" => "/queue",
        "/bg" => "/background",
        "/bp" => "/blueprint",
        "/goal" => "/objective",
        "/swarms" => "/swarm",
        "/question" => "/ask",
        "/autocompress" => "/autocompact",
        "/skins" => "/skin",
        "/summary" => "/recap",
        "/whoami" => "/profile",
        "/v" => "/version",
        "/billing" | "/credits" => "/usage",
        "/session" => "/sessions",
        "/switch" => "/sessions",
        "/sb" => "/statusbar",
        "/pilot" => "/autopilot",
        "/rb" => "/runbook",
        "/debug" => "/debug-dump",
        "/exit" => "/quit",
        "/suggest" => "/suggestions",
        other => other,
    }
}

pub fn render_command_catalog(
    filter: Option<&str>,
    commands: &'static [(&'static str, &'static str)],
) -> String {
    let query = filter.unwrap_or("").trim();
    let mut seen = HashSet::new();
    let mut out = String::new();
    out.push_str("Hermes Agent Ultra — Slash Command Palette\n");
    out.push_str("==========================================\n");
    if query.is_empty() {
        out.push_str(
            "Tip: type `/` in the composer to open completions and use arrows/Tab/Enter.\n",
        );
        out.push_str("Scoped search: `/commands <term>` (example: `/commands auth`).\n");
    } else {
        let _ = writeln!(out, "Filter: `{}`", query);
    }
    out.push('\n');

    for section in COMMAND_CATALOG_SECTIONS {
        let mut rendered = 0usize;
        for command in section.commands {
            let Some(description) = help_for(command, commands) else {
                continue;
            };
            if !command_catalog_matches_filter(command, description, query) {
                continue;
            }
            if rendered == 0 {
                let _ = writeln!(out, "## {}\n{}\n", section.title, section.hint);
            }
            let _ = writeln!(out, "- `{:<16}` {}", command, description);
            seen.insert(*command);
            rendered += 1;
        }
        if rendered > 0 {
            out.push('\n');
        }
    }

    let mut extras = Vec::new();
    for (command, description) in commands {
        if seen.contains(command) {
            continue;
        }
        if command_catalog_matches_filter(command, description, query) {
            extras.push((*command, *description));
        }
    }
    if !extras.is_empty() {
        out.push_str("## Other\nCommands that are available but not in the primary sections.\n\n");
        extras.sort_by(|a, b| a.0.cmp(b.0));
        for (command, description) in extras {
            let _ = writeln!(out, "- `{:<16}` {}", command, description);
        }
        out.push('\n');
    }
    out.push_str("You can also type plain text to send a normal chat message.");
    out
}

fn command_catalog_matches_filter(command: &str, description: &str, query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    let cmd = command.to_ascii_lowercase();
    let desc = description.to_ascii_lowercase();
    cmd.contains(&q) || desc.contains(&q.trim_start_matches('/'))
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

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[(&str, &str)] = &[
        ("/model", "Show/switch models"),
        ("/memory", "Show memory backend status"),
        ("/mcp", "List configured MCP servers"),
        ("/pilot", "Alias for /autopilot"),
    ];

    #[test]
    fn autocomplete_ranks_exact_and_fuzzy_matches() {
        assert_eq!(autocomplete("/model", COMMANDS).first(), Some(&"/model"));
        assert!(autocomplete("/mem", COMMANDS).contains(&"/memory"));
        assert!(autocomplete("/mdl", COMMANDS).contains(&"/model"));
    }

    #[test]
    fn canonical_command_maps_known_aliases() {
        assert_eq!(canonical_command("/pilot"), "/autopilot");
        assert_eq!(canonical_command("/clear"), "/new");
        assert_eq!(canonical_command("/custom"), "/custom");
    }

    #[test]
    fn render_command_catalog_filters_by_description() {
        let rendered = render_command_catalog(Some("memory"), COMMANDS);
        assert!(rendered.contains("Filter: `memory`"));
        assert!(rendered.contains("/memory"));
        assert!(!rendered.contains("/model"));
    }
}
