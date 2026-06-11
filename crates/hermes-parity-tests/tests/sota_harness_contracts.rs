use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
    let start = index.saturating_sub(500);
    let prefix = &source[start..index];
    prefix.contains("#[test]") || prefix.contains("#[tokio::test]")
}

#[test]
fn sota_harness_matrix_gate_passes_and_references_real_tests() {
    let matrix = read_json("docs/parity/sota-harness-matrix.json");
    assert_eq!(
        matrix["gate"]["pass"].as_bool(),
        Some(true),
        "SOTA harness gate must pass"
    );
    assert_eq!(
        matrix["gate"]["critical_gaps"].as_u64(),
        Some(0),
        "SOTA harness must have zero critical gaps"
    );
    assert_eq!(
        matrix["gate"]["missing_rust_test_refs"].as_u64(),
        Some(0),
        "SOTA harness must have zero missing Rust test refs"
    );
    assert_eq!(
        matrix["summary"]["domain_coverage_ratio"].as_f64(),
        Some(1.0),
        "all declared SOTA harness domains must be covered"
    );

    let domains = matrix["domains"].as_array().expect("domains array");
    assert_eq!(
        domains.len(),
        3,
        "matrix should govern exactly three domains"
    );

    let mut ids = BTreeSet::new();
    let mut direct_tests = 0_u64;
    for domain in domains {
        let id = domain["id"].as_str().expect("domain id");
        assert!(ids.insert(id.to_string()), "duplicate domain id {id}");
        assert_eq!(
            domain["status"].as_str(),
            Some("covered_by_rust_contracts"),
            "domain {id} must be backed by Rust contracts"
        );
        assert!(
            !domain["why"].as_str().unwrap_or_default().trim().is_empty(),
            "domain {id} must explain why it exists"
        );

        let fixtures = domain["fixtures"].as_array().expect("fixtures array");
        assert!(!fixtures.is_empty(), "domain {id} must cite fixtures");
        for fixture in fixtures {
            let path = fixture.as_str().expect("fixture path");
            assert!(
                repo_root().join(path).exists(),
                "domain {id} references missing fixture {path}"
            );
        }

        let tests = domain["rust_tests"].as_array().expect("rust_tests array");
        assert!(!tests.is_empty(), "domain {id} must cite Rust tests");
        direct_tests += tests.len() as u64;
        for test in tests {
            let file = test["file"].as_str().expect("test file");
            let name = test["name"].as_str().expect("test name");
            assert!(
                rust_test_exists(file, name),
                "domain {id} references missing or non-test Rust function {file}::{name}"
            );
        }
    }

    assert_eq!(
        matrix["summary"]["direct_rust_tests"].as_u64(),
        Some(direct_tests),
        "summary direct_rust_tests must match matrix references"
    );
    for required in [
        "workflow-replay-and-terminal-snapshots",
        "protocol-differential-contracts",
        "fault-injection-matrix",
    ] {
        assert!(
            ids.contains(required),
            "missing SOTA harness domain {required}"
        );
    }
}

#[test]
fn fault_injection_matrix_covers_required_classes() {
    let matrix = read_json("docs/parity/sota-harness-matrix.json");
    let chaos = read_json("crates/hermes-agent/src/testdata/adapter_chaos_profiles.json");

    let domain = matrix["domains"]
        .as_array()
        .expect("domains")
        .iter()
        .find(|d| d["id"].as_str() == Some("fault-injection-matrix"))
        .expect("fault domain");

    let required: BTreeSet<String> = domain["required_capabilities"]
        .as_array()
        .expect("required capabilities")
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    for required_capability in [
        "timeout recovery",
        "HTTP 5xx failover",
        "rate limit exhaustion",
        "connection reset recovery",
        "auth expiry fail-closed",
        "malformed tool payload retry without tools",
        "partial stream drop recovery",
    ] {
        assert!(
            required.contains(required_capability),
            "fault matrix missing required capability {required_capability}"
        );
    }

    let scenarios = chaos["scenarios"].as_array().expect("chaos scenarios");
    assert_eq!(
        matrix["summary"]["fault_scenarios"].as_u64(),
        Some(scenarios.len() as u64),
        "fault_scenarios summary must match chaos fixture"
    );

    let mut ids = BTreeSet::new();
    let mut step_kinds = BTreeSet::new();
    for scenario in scenarios {
        let id = scenario["id"].as_str().expect("scenario id");
        assert!(ids.insert(id.to_string()), "duplicate chaos scenario {id}");
        assert!(
            scenario["seed"].as_u64().is_some(),
            "scenario {id} must have deterministic seed"
        );
        assert!(
            scenario["expected"].is_object(),
            "scenario {id} must declare expected outcome"
        );
        for step in scenario["steps"].as_array().expect("steps") {
            if let Some(kind) = step["kind"].as_str() {
                step_kinds.insert(kind.to_string());
            }
        }
    }

    for required_kind in [
        "timeout",
        "http_5xx",
        "rate_limit",
        "connection_reset",
        "auth_expired",
        "malformed_tool_payload",
    ] {
        assert!(
            step_kinds.contains(required_kind),
            "chaos fixture missing step kind {required_kind}"
        );
    }

    for fixture in domain["fixtures"].as_array().expect("fixtures") {
        let path = repo_root().join(fixture.as_str().expect("fixture path"));
        assert!(
            Path::new(&path).exists(),
            "missing fixture {}",
            path.display()
        );
    }
}
