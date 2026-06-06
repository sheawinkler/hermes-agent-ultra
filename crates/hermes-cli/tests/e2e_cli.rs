use assert_cmd::Command;
use std::fs;

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
fn e2e_cli_profile_list_and_current_show_custom_alias() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();

    let mut create = Command::cargo_bin("hermes").expect("binary exists");
    create.env("HERMES_HOME", home);
    create.args(["profile", "create", "steve", "--no-alias"]);
    create.assert().success();

    let mut alias = Command::cargo_bin("hermes").expect("binary exists");
    alias.env("HERMES_HOME", home);
    alias.args(["profile", "alias", "steve", "--name", "qiaobusi"]);
    alias.assert().success();

    let mut list = Command::cargo_bin("hermes").expect("binary exists");
    list.env("HERMES_HOME", home);
    list.args(["profile", "list"]);
    let list_out = list.assert().success().get_output().stdout.clone();
    let list_text = std::str::from_utf8(&list_out).expect("utf8");
    assert!(
        list_text.contains("  steve (alias: qiaobusi)"),
        "expected custom alias in profile list, got: {list_text:?}"
    );

    let mut use_alias = Command::cargo_bin("hermes").expect("binary exists");
    use_alias.env("HERMES_HOME", home);
    use_alias.args(["profile", "use", "qiaobusi"]);
    use_alias.assert().success();

    let mut current = Command::cargo_bin("hermes").expect("binary exists");
    current.env("HERMES_HOME", home);
    current.arg("profile");
    let current_out = current.assert().success().get_output().stdout.clone();
    let current_text = std::str::from_utf8(&current_out).expect("utf8");
    assert!(
        current_text.contains("Active:      steve"),
        "expected resolved active profile, got: {current_text:?}"
    );
    assert!(
        current_text.contains("Alias:       qiaobusi"),
        "expected custom alias in current profile output, got: {current_text:?}"
    );
}

#[test]
fn e2e_cli_sessions_optimize_runs_against_isolated_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.args(["sessions", "optimize"]);
    let out = cmd.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("Optimizing session store"),
        "expected optimize banner, got: {text:?}"
    );
    assert!(
        text.contains("Optimized") && text.contains("FTS index"),
        "expected FTS index summary, got: {text:?}"
    );
    assert!(
        text.contains("Database size:"),
        "expected database size summary, got: {text:?}"
    );
    assert!(dir.path().join("sessions.db").exists());
}

#[test]
fn e2e_cli_interactive_without_tty_reports_actionable_diagnostic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.env_remove("HERMES_ALLOW_PARALLEL_INTERACTIVE");
    let assert = cmd.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("interactive Hermes requires a terminal (TTY)"),
        "expected TTY diagnostic in stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("hermes-ultra chat --query"),
        "expected non-interactive prompt guidance in stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("hermes-ultra doctor --deep --snapshot --bundle"),
        "expected doctor bundle guidance in stderr, got: {stderr:?}"
    );
}

#[test]
fn e2e_cli_systems_status_and_reports_are_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["systems", "status", "--json"],
        vec!["systems", "release", "--json"],
        vec!["systems", "agent-card", "card", "--json"],
        vec!["systems", "mcp", "conformance", "--json"],
        vec!["systems", "acp", "conformance", "--json"],
        vec!["systems", "providers", "--json"],
        vec!["systems", "handoff", "template", "--json"],
        vec!["systems", "provenance", "--json"],
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
fn e2e_cli_systems_replay_reports_existing_trace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let replay_dir = dir.path().join("logs").join("replay");
    fs::create_dir_all(&replay_dir).expect("mkdir replay");
    fs::write(
        replay_dir.join("session.jsonl"),
        concat!(
            "{\"seq\":1,\"event\":\"start\",\"trace_id\":\"t\",\"prev_hash\":null,\"event_hash\":\"a\"}\n",
            "{\"seq\":2,\"event\":\"stop\",\"trace_id\":\"t\",\"prev_hash\":\"a\",\"event_hash\":\"b\"}\n"
        ),
    )
    .expect("write replay");

    let mut show = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
    show.env("HERMES_HOME", dir.path());
    show.args(["systems", "replay", "--json"]);
    let out = show.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    let report: serde_json::Value = serde_json::from_str(text).expect("show json");
    assert_eq!(report.get("log_count").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(report.get("passed").and_then(|v| v.as_bool()), Some(true));
}
