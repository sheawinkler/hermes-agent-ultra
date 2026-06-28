use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    let path =
        repo_root().join("crates/hermes-source-parity-tests/tests/fixtures/command_actions.json");
    let raw = std::fs::read_to_string(&path).expect("read command_actions fixture");
    serde_json::from_str(&raw).expect("parse command_actions fixture")
}

fn read_source(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap_or_else(|err| panic!("read {path}: {err}"))
}

fn read_included_source_tree(root: &Path, entry_path: &str, _include_dir: &str) -> String {
    fn append_source(
        root: &Path,
        relative_path: &Path,
        include_re: &Regex,
        visited: &mut BTreeSet<PathBuf>,
        combined: &mut String,
    ) {
        if !visited.insert(relative_path.to_path_buf()) {
            return;
        }
        let source = read_source(root, &relative_path.to_string_lossy());
        combined.push_str("\n\n");
        combined.push_str(&source);

        let Some(parent) = relative_path.parent() else {
            return;
        };
        for cap in include_re.captures_iter(&source) {
            let include_name = &cap[1];
            let include_path = parent.join(include_name);
            append_source(root, &include_path, include_re, visited, combined);
        }
    }

    let include_re =
        Regex::new(r#"include!\("([^"]+\.rs)"\);"#).expect("include regex should compile");
    let mut combined = String::new();
    let mut visited = BTreeSet::new();
    append_source(
        root,
        Path::new(entry_path),
        &include_re,
        &mut visited,
        &mut combined,
    );
    combined
}

fn read_main_module_sources(root: &Path) -> String {
    read_included_source_tree(
        root,
        "crates/hermes-cli/src/main.rs",
        "crates/hermes-cli/src",
    )
}

fn read_commands_module_sources(root: &Path) -> String {
    read_included_source_tree(
        root,
        "crates/hermes-cli/src/commands/mod.rs",
        "crates/hermes-cli/src/commands",
    )
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

fn normalize_variant_command_name(variant: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in variant.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if idx > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn extract_top_level_commands_from_cli_source(source: &str) -> BTreeSet<String> {
    let variant_re =
        Regex::new(r"^    ([A-Z][A-Za-z0-9_]*)(?:\s*[,{(]|,)$").expect("variant regex");
    let command_name_re =
        Regex::new(r#"#\[command\(name\s*=\s*"([^"]+)""#).expect("command name regex");
    let mut in_enum = false;
    let mut explicit_name: Option<String> = None;
    let mut out = BTreeSet::new();

    for line in source.lines() {
        if line.trim() == "pub enum CliCommand {" {
            in_enum = true;
            continue;
        }
        if !in_enum {
            continue;
        }
        if line == "}" {
            break;
        }
        if let Some(cap) = command_name_re.captures(line.trim()) {
            explicit_name = Some(cap[1].to_string());
            continue;
        }
        let Some(cap) = variant_re.captures(line) else {
            continue;
        };
        let variant = &cap[1];
        let name = explicit_name
            .take()
            .unwrap_or_else(|| normalize_variant_command_name(variant));
        out.insert(name);
    }
    out
}

#[test]
fn cli_top_level_surface_contains_required_commands() {
    let fixture = load_fixture();
    let source = std::fs::read_to_string(repo_root().join("crates/hermes-cli/src/cli.rs"))
        .expect("read cli.rs");
    let have = extract_top_level_commands_from_cli_source(&source);

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
        ("main", read_main_module_sources(&root)),
        ("commands", read_commands_module_sources(&root)),
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
    let path =
        repo_root().join("crates/hermes-source-parity-tests/tests/fixtures/command_actions.json");
    assert!(
        Path::new(&path).exists(),
        "missing fixture at {}",
        path.display()
    );
}
