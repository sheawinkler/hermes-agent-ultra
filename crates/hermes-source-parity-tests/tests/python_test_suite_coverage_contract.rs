use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_text(path: &str) -> String {
    let full = repo_root().join(path);
    std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed reading {}: {}", full.display(), e))
}

fn read_json(path: &str) -> Value {
    let raw = read_text(path);
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("failed parsing {path}: {e}"))
}

fn rust_test_exists(file: &str, name: &str) -> bool {
    let source = read_text(file);
    let needle = format!("fn {name}");
    let Some(index) = source.find(&needle) else {
        return false;
    };
    let start = index.saturating_sub(500);
    let prefix = &source[start..index];
    prefix.contains("#[test]") || prefix.contains("#[tokio::test")
}

#[test]
fn python_test_suite_manifest_covers_all_backlog_rows_and_references_real_tests() {
    let manifest = read_json("docs/parity/python-test-suite-coverage.json");
    let backlog = read_json("docs/parity/shared-diff-backlog.json");

    let manifest_entries = manifest["entries"]
        .as_array()
        .expect("manifest entries should be array");
    assert_eq!(
        manifest["summary"]["covered_paths"].as_u64(),
        Some(manifest_entries.len() as u64),
        "manifest summary covered_paths must match entries length"
    );

    let manifest_paths: BTreeSet<String> = manifest_entries
        .iter()
        .map(|entry| entry["path"].as_str().expect("manifest path").to_string())
        .collect();
    assert_eq!(
        manifest_paths.len(),
        manifest_entries.len(),
        "manifest paths must be unique"
    );

    let required_prefixes: BTreeSet<String> = manifest["summary"]
        ["required_backlog_classification_paths"]
        .as_array()
        .expect("required_backlog_classification_paths should be array")
        .iter()
        .map(|value| value.as_str().expect("classification path").to_string())
        .collect();
    assert_eq!(
        required_prefixes,
        BTreeSet::from_iter([
            "tests".to_string(),
            "tests/agent".to_string(),
            "tests/cli".to_string(),
            "tests/e2e".to_string(),
            "tests/honcho_plugin".to_string(),
            "tests/integration".to_string(),
            "tests/plugins".to_string(),
            "tests/run_agent".to_string(),
            "tests/skills".to_string(),
            "tests/stress".to_string(),
            "tests/tools".to_string(),
            "tests/tui_gateway".to_string(),
        ]),
        "manifest must explicitly own the final Python test-suite backlog prefixes"
    );

    let backlog_paths: BTreeSet<String> = backlog["entries"]
        .as_array()
        .expect("backlog entries should be array")
        .iter()
        .filter(|entry| {
            entry["classification_path"]
                .as_str()
                .is_some_and(|path| required_prefixes.contains(path))
        })
        .map(|entry| entry["path"].as_str().expect("backlog path").to_string())
        .collect();

    assert_eq!(
        manifest_paths, backlog_paths,
        "Python test-suite coverage manifest must cover exactly the final backlog rows"
    );

    for entry in manifest_entries {
        let path = entry["path"].as_str().expect("manifest path");
        assert_eq!(
            entry["status"].as_str(),
            Some("covered_by_rust_test_suite_contracts"),
            "{path} should be marked covered_by_rust_test_suite_contracts"
        );
        assert!(
            !entry["rationale"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .is_empty(),
            "{path} must explain why the Python test row is covered"
        );
        let tests = entry["rust_tests"]
            .as_array()
            .expect("rust_tests should be array");
        assert!(!tests.is_empty(), "{path} must cite at least one Rust test");
        for test in tests {
            let file = test["file"].as_str().expect("rust test file");
            let name = test["name"].as_str().expect("rust test name");
            assert!(
                rust_test_exists(file, name),
                "{path} references missing or non-test Rust function {file}::{name}"
            );
        }
    }
}

#[test]
fn python_suite_top_level_install_packaging_contracts_are_rust_owned() {
    let install = read_text("scripts/install.sh");
    assert!(install.contains("CANONICAL_BIN_NAME=\"${CANONICAL_BIN_NAME:-hermes-agent-ultra}\""));
    assert!(install.contains("PRIMARY_BIN_NAME=\"${PRIMARY_BIN_NAME:-hermes-ultra}\""));
    assert!(install.contains("INSTALL_LEGACY_ALIAS=\"${INSTALL_LEGACY_ALIAS:-false}\""));
    assert!(install.contains("-u PYTHONHOME"));
    assert!(install.contains("-u PYTHONPATH"));
    assert!(install.contains("cargo build --release -p hermes-cli --bin \"${CANONICAL_BIN_NAME}\""));
    let legacy_gate = install
        .find("truthy_env \"${INSTALL_LEGACY_ALIAS}\"")
        .expect("legacy alias gate");
    let legacy_link = install
        .find("ln -sfn \"${CANONICAL_BIN_NAME}\" \"${INSTALL_DIR}/${LEGACY_BIN_NAME}\"")
        .expect("legacy alias symlink");
    assert!(legacy_gate < legacy_link);

    let readme = read_text("README.md");
    assert!(readme.contains("--bin hermes-agent-ultra --bin hermes-ultra"));
    assert!(!readme.contains("--bin hermes\n"));
    assert!(readme.contains("hermes-ultra setup"));

    let cli_toml = read_text("crates/hermes-cli/Cargo.toml");
    assert!(cli_toml.contains("name = \"hermes-ultra\""));
    assert!(cli_toml.contains("name = \"hermes-agent-ultra\""));

    let formula = read_text("packaging/homebrew/hermes-agent.rb");
    for asset in [
        "hermes-macos-aarch64.tar.gz",
        "hermes-macos-x86_64.tar.gz",
        "hermes-linux-aarch64.tar.gz",
        "hermes-linux-x86_64.tar.gz",
    ] {
        assert!(
            formula.contains(asset),
            "Homebrew formula missing asset {asset}"
        );
    }
    assert!(formula.contains("bin.install \"hermes\" => \"hermes-agent-ultra\""));
    assert!(formula.contains("bin.install_symlink \"hermes-agent-ultra\" => \"hermes-ultra\""));
}

#[test]
fn python_suite_skill_and_external_service_contracts_are_rust_owned() {
    let skill_sync = read_text("crates/hermes-skills/src/sync.rs");
    assert!(skill_sync.contains("google-workspace"));
    assert!(skill_sync.contains("reset_bundled_skill"));
    assert!(skill_sync.contains("fresh_sync_copies_records_hashes_and_category_description"));

    let claw = read_text("crates/hermes-cli/src/claw_migrate.rs");
    assert!(claw.contains("OpenClaw"));
    assert!(claw.contains("OPENCLAW_DIR_NAMES"));
    assert!(claw.contains("PERSONALITY_FILES"));
    assert!(claw.contains("test_find_openclaw_dir_explicit"));

    let credential_files = read_text("crates/hermes-tools/src/tools/credential_files.rs");
    assert!(credential_files.contains("google_token.json"));
    assert!(credential_files.contains("credential_mount_accepts_path_name_and_string_entries"));
    assert!(
        credential_files.contains("credential_mount_rejects_traversal_absolute_and_symlink_escape")
    );

    let teams = read_text("crates/hermes-tools/src/teams_pipeline.rs");
    assert!(teams.contains("MicrosoftGraphTokenProvider"));
    assert!(teams.contains("TeamsMeetingPipeline"));
    assert!(teams.contains("ffmpeg_extract_audio"));
    assert!(teams.contains("transcript_first_path_persists_state"));

    let telemetry = read_text("crates/hermes-telemetry/src/lib.rs");
    assert!(telemetry.contains("langfuse_trace_config_from_env"));
    assert!(telemetry.contains("x-langfuse-ingestion-version"));
    assert!(telemetry.contains("langfuse_config_resolves_endpoint_headers_and_resource_attrs"));
}

#[test]
fn python_suite_mcp_environment_and_stress_contracts_are_rust_owned() {
    let mcp_serve = read_text("crates/hermes-mcp/src/serve.rs");
    assert!(mcp_serve.contains("test_hermes_mcp_serve_tool_defs"));
    assert!(mcp_serve.contains("test_numeric_args_coerce_from_strings"));
    assert!(mcp_serve.contains("test_event_bridge_queue_limit"));

    let terminal_requirements = read_text("crates/hermes-tools/src/terminal_requirements.rs");
    for backend in ["docker", "singularity", "modal", "daytona", "ssh"] {
        assert!(
            terminal_requirements.contains(backend),
            "missing backend {backend}"
        );
    }
    assert!(terminal_requirements.contains("modal_managed_mode_requires_managed_gateway"));
    assert!(terminal_requirements.contains("ssh_requires_host_and_user"));

    let docker = read_text("crates/hermes-environments/src/docker.rs");
    assert!(docker.contains("build_run_args_include_terminal_config_env_and_resource_flags"));
    assert!(docker.contains("docker_forward_env"));
    assert!(docker.contains("docker_volumes"));

    let file_sync = read_text("crates/hermes-environments/src/file_sync.rs");
    assert!(file_sync.contains("sync_from_remote_uses_atomic_local_write"));
    assert!(file_sync.contains("atomic_write_text_replaces_existing_file_and_cleans_temp"));

    let gateway_contracts = read_text("crates/hermes-gateway/tests/contract_primary_platforms.rs");
    assert!(
        gateway_contracts.contains("contract_reconnect_watcher_restarts_offline_primary_adapter")
    );
    assert!(gateway_contracts.contains("contract_duplicate_message_id_redelivery_is_suppressed"));
}
