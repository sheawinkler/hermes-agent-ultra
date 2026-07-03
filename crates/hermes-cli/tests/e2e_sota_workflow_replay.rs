use assert_cmd::Command;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ReplaySuite {
    schema_version: u32,
    suite: String,
    workflows: Vec<Workflow>,
}

#[derive(Debug, Deserialize)]
struct Workflow {
    id: String,
    purpose: String,
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
struct Step {
    id: String,
    args: Vec<String>,
    #[serde(default = "default_success")]
    expect_success: bool,
    snapshot_role: String,
    #[serde(default)]
    expected_stdout_contains: Vec<String>,
    #[serde(default)]
    expected_stderr_contains: Vec<String>,
}

fn default_success() -> bool {
    true
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_suite() -> ReplaySuite {
    let path =
        repo_root().join("crates/hermes-parity-tests/tests/fixtures/sota_workflow_replay.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed reading {}: {}", path.display(), err));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed parsing {}: {}", path.display(), err))
}

fn normalized_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\\', "/")
}

#[test]
fn sota_workflow_replay_fixture_is_well_formed() {
    let suite = load_suite();
    assert_eq!(suite.schema_version, 1);
    assert_eq!(suite.suite, "sota_workflow_replay");
    assert!(
        suite.workflows.len() >= 2,
        "expected CLI and PTY/terminal diagnostic workflows"
    );

    let mut workflow_ids = std::collections::BTreeSet::new();
    let mut step_ids = std::collections::BTreeSet::new();
    let mut roles = std::collections::BTreeSet::new();
    for workflow in &suite.workflows {
        assert!(
            workflow_ids.insert(workflow.id.as_str()),
            "duplicate workflow id {}",
            workflow.id
        );
        assert!(
            !workflow.purpose.trim().is_empty(),
            "workflow {} must explain its purpose",
            workflow.id
        );
        assert!(
            !workflow.steps.is_empty(),
            "workflow {} has no steps",
            workflow.id
        );
        for step in &workflow.steps {
            assert!(
                step_ids.insert(step.id.as_str()),
                "duplicate step id {}",
                step.id
            );
            assert!(
                !step.snapshot_role.trim().is_empty(),
                "step {} has no snapshot_role",
                step.id
            );
            roles.insert(step.snapshot_role.as_str());
            assert!(
                !step.expected_stdout_contains.is_empty()
                    || !step.expected_stderr_contains.is_empty(),
                "step {} has no observable snapshot assertions",
                step.id
            );
        }
    }

    for required in [
        "cli_status",
        "tool_registry",
        "auth_status",
        "gateway_status",
        "release_systems_status",
        "release_gate",
        "provider_qos",
        "memory_fusion",
        "pty_diagnostic",
    ] {
        assert!(roles.contains(required), "missing snapshot role {required}");
    }
}

#[test]
fn sota_workflow_replay_executes_cli_journey_snapshots() {
    let suite = load_suite();
    let home = tempfile::tempdir().expect("temp hermes home");

    for workflow in suite.workflows {
        for step in workflow.steps {
            let mut cmd = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
            cmd.env("HERMES_HOME", home.path());
            if step.id == "interactive_without_tty" {
                cmd.env_remove("HERMES_ALLOW_PARALLEL_INTERACTIVE");
            }
            cmd.args(&step.args);

            let assert = if step.expect_success {
                cmd.assert().success()
            } else {
                cmd.assert().failure()
            };
            let output = assert.get_output();
            let stdout = normalized_text(&output.stdout);
            let stderr = normalized_text(&output.stderr);

            for expected in &step.expected_stdout_contains {
                assert!(
                    stdout.contains(expected),
                    "workflow={} step={} stdout missing {:?}; stdout={:?}",
                    workflow.id,
                    step.id,
                    expected,
                    stdout
                );
            }
            for expected in &step.expected_stderr_contains {
                assert!(
                    stderr.contains(expected),
                    "workflow={} step={} stderr missing {:?}; stderr={:?}",
                    workflow.id,
                    step.id,
                    expected,
                    stderr
                );
            }
        }
    }
}
