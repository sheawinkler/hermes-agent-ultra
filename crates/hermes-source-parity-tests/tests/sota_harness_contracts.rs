use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

mod support;
use support::rust_test_exists;

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

#[test]
fn fixture_reference_matrix_gate_passes_and_references_real_tests() {
    let matrix = read_json("docs/parity/sota-harness-matrix.json");
    assert_eq!(
        matrix["authority"]["capability_promotion_authoritative"].as_bool(),
        Some(false),
        "fixture/reference coverage must not authorize capability promotion"
    );
    assert_eq!(
        matrix["authority"]["production_complete"].as_bool(),
        Some(false),
        "fixture/reference coverage must not claim production completeness"
    );
    assert_eq!(
        matrix["scope"]["coverage_kind"].as_str(),
        Some("fixture_reference_regression"),
        "matrix scope must identify fixture/reference regression coverage"
    );
    assert_eq!(
        matrix["gate"]["scope"].as_str(),
        Some("fixture_reference_regression"),
        "matrix gate must remain scoped to fixture/reference regression"
    );
    assert_eq!(
        matrix["gate"]["pass"].as_bool(),
        Some(true),
        "fixture/reference regression gate must pass"
    );
    assert_eq!(
        matrix["gate"]["critical_gaps"].as_u64(),
        Some(0),
        "fixture/reference matrix must have zero unresolved fixture-reference gaps"
    );
    assert_eq!(
        matrix["gate"]["missing_rust_test_refs"].as_u64(),
        Some(0),
        "fixture/reference matrix must have zero missing referenced Rust test refs"
    );
    assert_eq!(
        matrix["summary"]["domain_coverage_ratio"].as_f64(),
        Some(1.0),
        "all declared fixture-reference categories must be represented"
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
            Some("fixture_reference_regression_coverage"),
            "domain {id} must be backed by fixture/reference regression coverage"
        );
        assert!(
            !domain["why"].as_str().unwrap_or_default().trim().is_empty(),
            "domain {id} must explain why it exists"
        );

        let categories = domain["fixture_reference_categories"]
            .as_array()
            .expect("fixture_reference_categories array");
        assert!(
            !categories.is_empty(),
            "domain {id} must cite fixture/reference categories"
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
            "missing fixture/reference matrix domain {required}"
        );
    }
}

#[test]
fn contextlattice_index_retains_fixture_reference_non_authoritative_scope() {
    let matrix = read_json("docs/parity/sota-harness-matrix.json");
    let index = read_json("docs/parity/contextlattice-replay-evidence-index.json");

    assert_eq!(
        index["authority"]["capability_promotion_authoritative"].as_bool(),
        Some(false),
        "ContextLattice must retain non-authoritative capability scope"
    );
    assert_eq!(
        index["authority"]["production_complete"].as_bool(),
        Some(false),
        "ContextLattice must not promote fixture/reference coverage to production complete"
    );
    assert_eq!(
        index["scope"]["coverage_kind"].as_str(),
        Some("fixture_reference_regression"),
        "ContextLattice scope must identify fixture/reference regression coverage"
    );
    assert_eq!(
        index["latest_gate_state"]["production_complete"].as_bool(),
        Some(false),
        "ContextLattice gate state must not promote fixture/reference coverage"
    );

    let groups = index["replay_evidence_groups"]
        .as_array()
        .expect("ContextLattice replay evidence groups");
    for domain in matrix["domains"].as_array().expect("matrix domains") {
        let id = domain["id"].as_str().expect("domain id");
        let group = groups
            .iter()
            .find(|group| group["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("ContextLattice missing domain {id}"));
        assert_eq!(
            group["status"], domain["status"],
            "ContextLattice status must preserve fixture/reference scope for {id}"
        );
        assert_eq!(
            group["fixtures"], domain["fixtures"],
            "ContextLattice fixtures must match matrix for {id}"
        );
        assert_eq!(
            group["fixture_reference_categories"],
            domain["fixture_reference_categories"],
            "ContextLattice categories must match matrix for {id}"
        );
        assert_eq!(
            group["rust_tests"], domain["rust_tests"],
            "ContextLattice Rust test references must match matrix for {id}"
        );
        assert!(
            group.get("required_capabilities").is_none(),
            "ContextLattice must not relabel fixture/reference categories as capabilities"
        );
    }

    for (name, path) in [
        ("matrix", "docs/parity/sota-harness-matrix.md"),
        (
            "ContextLattice index",
            "docs/parity/contextlattice-replay-evidence-index.md",
        ),
    ] {
        let markdown = std::fs::read_to_string(repo_root().join(path))
            .unwrap_or_else(|err| panic!("failed reading {name} markdown: {err}"));
        assert!(
            markdown.contains("Fixture-Reference"),
            "{name} markdown must identify fixture-reference scope"
        );
        assert!(
            markdown.contains("Production complete: `false`"),
            "{name} markdown must declare production_complete=false"
        );
    }
}

#[test]
fn fault_injection_fixture_reference_matrix_covers_required_categories() {
    let matrix = read_json("docs/parity/sota-harness-matrix.json");
    let chaos = read_json("crates/hermes-agent/src/testdata/adapter_chaos_profiles.json");

    let domain = matrix["domains"]
        .as_array()
        .expect("domains")
        .iter()
        .find(|d| d["id"].as_str() == Some("fault-injection-matrix"))
        .expect("fault domain");

    let required: BTreeSet<String> = domain["fixture_reference_categories"]
        .as_array()
        .expect("fixture reference categories")
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
            "fault fixture-reference matrix missing required category {required_capability}"
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
