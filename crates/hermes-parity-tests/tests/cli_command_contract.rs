use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CliCommandContractFixture {
    required_top_level: Vec<String>,
    required_actions: BTreeMap<String, Vec<String>>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_fixture() -> CliCommandContractFixture {
    let path = repo_root().join("crates/hermes-parity-tests/tests/fixtures/command_actions.json");
    let raw = std::fs::read_to_string(&path).expect("read command_actions fixture");
    serde_json::from_str(&raw).expect("parse command_actions fixture")
}

fn function_body<'a>(source: &'a str, fn_name: &str) -> Option<&'a str> {
    let fn_re = Regex::new(&format!(
        r"(?m)^(?:pub\s+)?(?:async\s+)?fn\s+{}\b",
        regex::escape(fn_name)
    ))
    .ok()?;
    let m = fn_re.find(source)?;
    let start = m.start();
    let next_fn_re =
        Regex::new(r"(?m)^(?:pub\s+)?(?:async\s+)?fn\s+[A-Za-z0-9_]+").expect("fn boundary regex");
    let end = next_fn_re
        .find_at(source, m.end())
        .map(|n| n.start())
        .unwrap_or(source.len());
    Some(&source[start..end])
}

fn extract_actions_from_function(source: &str, fn_name: &str) -> BTreeSet<String> {
    let body =
        function_body(source, fn_name).unwrap_or_else(|| panic!("function not found: {}", fn_name));
    let quoted = Regex::new(r#"\"([A-Za-z0-9_-]+)\""#).expect("regex compile");
    let default_re =
        Regex::new(r#"unwrap_or\("([A-Za-z0-9_-]+)"\)"#).expect("default regex compile");
    let mut out = BTreeSet::new();

    for cap in default_re.captures_iter(body) {
        out.insert(cap[1].to_string());
    }

    for line in body.lines() {
        let t = line.trim();
        if !t.contains("=>") {
            continue;
        }
        if !(t.starts_with("Some(") || t.starts_with("None") || t.starts_with('"')) {
            continue;
        }
        for cap in quoted.captures_iter(t) {
            out.insert(cap[1].to_string());
        }
    }
    out
}

#[test]
fn cli_top_level_surface_contains_required_commands() {
    let fixture = load_fixture();
    let command = hermes_cli::Cli::command();
    let have: BTreeSet<String> = command
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();

    for required in fixture.required_top_level {
        assert!(
            have.contains(&required),
            "missing top-level command '{}'; have {:?}",
            required,
            have
        );
    }
}

#[test]
fn cli_action_contract_matches_fixture() {
    let fixture = load_fixture();
    let root = repo_root();

    let sources: BTreeMap<&str, String> = [
        (
            "main",
            std::fs::read_to_string(root.join("crates/hermes-cli/src/main.rs"))
                .expect("read main.rs"),
        ),
        (
            "commands",
            std::fs::read_to_string(root.join("crates/hermes-cli/src/commands.rs"))
                .expect("read commands.rs"),
        ),
    ]
    .into_iter()
    .collect();

    let action_sources: BTreeMap<&str, (&str, &str)> = [
        ("tools", ("main", "run_tools")),
        ("gateway", ("main", "run_gateway")),
        ("auth", ("main", "run_auth")),
        ("cron", ("main", "run_cron")),
        ("webhook", ("main", "run_webhook")),
        ("profile", ("main", "run_profile")),
        ("memory", ("commands", "handle_cli_memory")),
        ("mcp", ("commands", "handle_cli_mcp")),
        ("skills", ("commands", "handle_cli_skills")),
    ]
    .into_iter()
    .collect();

    for (command, expected_actions) in fixture.required_actions {
        let (source_key, function_name) = action_sources
            .get(command.as_str())
            .unwrap_or_else(|| panic!("missing source mapping for '{}'", command));
        let source = sources
            .get(source_key)
            .unwrap_or_else(|| panic!("missing source blob for '{}'", source_key));
        let have = extract_actions_from_function(source, function_name);
        let missing: Vec<String> = expected_actions
            .iter()
            .filter(|a| !have.contains(*a))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "command '{}' missing actions {:?}; extracted actions={:?}",
            command,
            missing,
            have
        );
    }
}

#[test]
fn fixture_path_is_stable() {
    let path = repo_root().join("crates/hermes-parity-tests/tests/fixtures/command_actions.json");
    assert!(
        Path::new(&path).exists(),
        "missing fixture at {}",
        path.display()
    );
}
