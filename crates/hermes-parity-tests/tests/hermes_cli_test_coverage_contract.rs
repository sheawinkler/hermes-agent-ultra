use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &str) -> Value {
    let full = repo_root().join(path);
    let raw = std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed reading {}: {}", full.display(), e));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed parsing {}: {}", full.display(), e))
}

fn rust_test_exists(file: &str, name: &str) -> bool {
    let full = repo_root().join(file);
    let source = std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed reading referenced Rust file {}: {}", file, e));
    let needle = format!("fn {name}");
    let Some(index) = source.find(&needle) else {
        return false;
    };
    let start = index.saturating_sub(400);
    let prefix = &source[start..index];
    prefix.contains("#[test]") || prefix.contains("#[tokio::test]")
}

#[test]
fn hermes_cli_manifest_covers_all_backlog_rows_and_references_real_tests() {
    let manifest = read_json("docs/parity/hermes-cli-test-coverage.json");
    let backlog = read_json("docs/parity/shared-diff-backlog.json");

    let manifest_entries = manifest["entries"]
        .as_array()
        .expect("entries should be array");
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

    let backlog_paths: BTreeSet<String> = backlog["entries"]
        .as_array()
        .expect("backlog entries should be array")
        .iter()
        .filter(|entry| entry["classification_path"].as_str() == Some("tests/hermes_cli"))
        .map(|entry| entry["path"].as_str().expect("backlog path").to_string())
        .collect();

    assert_eq!(
        manifest_paths, backlog_paths,
        "Hermes CLI coverage manifest must cover exactly the tests/hermes_cli backlog rows"
    );

    for entry in manifest_entries {
        let path = entry["path"].as_str().expect("manifest path");
        assert_eq!(
            entry["status"].as_str(),
            Some("covered_by_rust_contracts"),
            "{path} should be marked covered_by_rust_contracts"
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
