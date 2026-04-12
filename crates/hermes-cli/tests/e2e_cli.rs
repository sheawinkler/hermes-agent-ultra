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
    assert!(text.contains("***"), "expected masked api key, got: {text:?}");
}
