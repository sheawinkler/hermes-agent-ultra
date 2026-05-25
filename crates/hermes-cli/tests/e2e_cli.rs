use assert_cmd::Command;
use std::fs;
use std::process::Command as StdCommand;

#[test]
fn e2e_cli_model_command_prints_current_model() {
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.arg("model");
    cmd.assert().success();
}

#[test]
fn e2e_cli_gateway_status_command_runs() {
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.args(["gateway", "status"]);
    cmd.assert().success();
}

#[test]
fn e2e_cli_tools_list_shows_registered_tools() {
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.args(["tools", "list"]);
    let out = cmd.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("send_message"),
        "expected send_message in tools list: {text:?}"
    );
    assert!(
        text.contains("clarify"),
        "expected clarify in tools list: {text:?}"
    );
}

#[test]
fn e2e_cli_config_set_persists_model_to_yaml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.args(["config", "set", "model", "openai:gpt-4o-mini"]);
    cmd.assert().success();
    let cfg = dir.path().join("config.yaml");
    assert!(cfg.exists(), "config.yaml should be created");
    let raw = fs::read_to_string(&cfg).expect("read config");
    assert!(
        raw.contains("gpt-4o-mini"),
        "expected model in yaml: {}",
        raw
    );
}

#[test]
fn e2e_cli_config_set_dotted_llm_and_get_masks_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let mut set = Command::cargo_bin("hermes").expect("binary exists");
    set.env("HERMES_HOME", home);
    set.args(["config", "set", "llm.openai.api_key", "sk-testkey123456"]);
    set.assert().success();

    let mut get = Command::cargo_bin("hermes").expect("binary exists");
    get.env("HERMES_HOME", home);
    get.args(["config", "get", "llm.openai.api_key"]);
    let out = get.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("***"),
        "expected masked api key, got: {text:?}"
    );
}

#[test]
fn e2e_cli_interactive_refuses_parallel_session_when_lock_pid_is_alive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_path = dir.path().join("interactive.session.lock");
    let mut sleeper = StdCommand::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep process");

    fs::write(&lock_path, format!("{}\n", sleeper.id())).expect("write lock");

    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.env_remove("HERMES_ALLOW_PARALLEL_INTERACTIVE");
    let assert = cmd.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("Another Hermes interactive session is running"),
        "expected lock guard error in stderr, got: {stderr:?}"
    );

    let _ = sleeper.kill();
    let _ = sleeper.wait();
}

#[test]
fn e2e_cli_sota_status_and_reports_are_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["sota", "status", "--json"],
        vec!["sota", "eval", "--json"],
        vec!["sota", "a2a", "card", "--json"],
        vec!["sota", "mcp", "conformance", "--json"],
        vec!["sota", "capabilities", "--json"],
        vec!["sota", "handoff", "template", "--json"],
    ] {
        let mut cmd = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
        cmd.env("HERMES_HOME", dir.path());
        cmd.args(args);
        let out = cmd.assert().success().get_output().stdout.clone();
        let text = std::str::from_utf8(&out).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|err| panic!("json parse failed: {err}; {text}"));
        assert!(
            value.get("kind").is_some() || value.get("name").is_some(),
            "expected report/card json, got {value}"
        );
    }
}

#[test]
fn e2e_cli_sota_flight_sample_persists_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut sample = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
    sample.env("HERMES_HOME", dir.path());
    sample.args(["sota", "flight", "sample", "--json"]);
    let out = sample.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    let report: serde_json::Value = serde_json::from_str(text).expect("sample json");
    assert_eq!(report.get("event_count").and_then(|v| v.as_u64()), Some(1));

    let mut show = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
    show.env("HERMES_HOME", dir.path());
    show.args(["sota", "flight", "show", "--json"]);
    let out = show.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    let report: serde_json::Value = serde_json::from_str(text).expect("show json");
    assert_eq!(report.get("event_count").and_then(|v| v.as_u64()), Some(1));
}
