use assert_cmd::Command;
use std::fs;

#[test]
fn e2e_cli_model_command_prints_current_model() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("config.yaml"),
        r#"
model: local-test:deterministic
llm_providers:
  local-test:
    base_url: http://127.0.0.1:9/v1
    discover_models: false
    models:
      deterministic: {}
"#,
    )
    .expect("write config");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.arg("model");
    cmd.assert().success();
}

#[test]
fn e2e_cli_model_command_lists_configured_custom_provider_models() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("config.yaml"),
        r#"
model: qianfan-coding:kimi-k2.5
llm_providers:
  qianfan-coding:
    base_url: https://qianfan.baidubce.com/v2/coding
    discover_models: false
    models:
      kimi-k2.5: {}
      glm-5: {}
"#,
    )
    .expect("write config");

    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.arg("model");
    let out = cmd.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("qianfan-coding"),
        "expected custom provider in model list: {text:?}"
    );
    assert!(
        text.contains("kimi-k2.5") && text.contains("glm-5"),
        "expected configured model subset in model list: {text:?}"
    );
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
fn e2e_cli_config_set_get_and_show_never_echo_url_credentials() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let password = "proxy-password-sentinel";
    let query_secret = "proxy-query-sentinel";
    let proxy = format!("http://alice:{password}@proxy.example.test:8080?api_key={query_secret}");

    let mut set = Command::cargo_bin("hermes").expect("binary exists");
    set.env("HERMES_HOME", home);
    for key in [
        "HERMES_PROXY_HTTP",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ] {
        set.env_remove(key);
    }
    set.args(["config", "set", "proxy.http", proxy.as_str()]);
    let set_output = set.assert().success().get_output().clone();

    let raw = fs::read_to_string(home.join("config.yaml")).expect("read config");
    assert!(raw.contains(password), "password should persist to config");
    assert!(
        raw.contains(query_secret),
        "query credential should persist to config"
    );

    let mut get = Command::cargo_bin("hermes").expect("binary exists");
    get.env("HERMES_HOME", home);
    for key in [
        "HERMES_PROXY_HTTP",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ] {
        get.env_remove(key);
    }
    get.args(["config", "get", "proxy.http"]);
    let get_output = get.assert().success().get_output().clone();

    let mut show = Command::cargo_bin("hermes").expect("binary exists");
    show.env("HERMES_HOME", home);
    for key in [
        "HERMES_PROXY_HTTP",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ] {
        show.env_remove(key);
    }
    show.args(["config", "show"]);
    let show_output = show.assert().success().get_output().clone();
    for (surface, output) in [("get", &get_output), ("show", &show_output)] {
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("proxy.example.test"),
            "{surface} did not exercise the persisted proxy: {stdout:?}"
        );
        assert!(
            stdout.contains("REDACTED"),
            "{surface} did not render a redacted proxy: {stdout:?}"
        );
    }

    for (surface, output) in [
        ("set", set_output),
        ("get", get_output),
        ("show", show_output),
    ] {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for sentinel in [password, query_secret] {
            assert!(
                !stdout.contains(sentinel),
                "{surface} stdout leaked {sentinel}: {stdout:?}"
            );
            assert!(
                !stderr.contains(sentinel),
                "{surface} stderr leaked {sentinel}: {stderr:?}"
            );
        }
    }
}

#[test]
fn e2e_cli_config_model_url_credentials_never_reach_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let user = "model-user-sentinel";
    let password = "model-password-sentinel";
    let query = "model-query-sentinel";
    let model = format!("https://{user}:{password}@models.example.test/v1?api_key={query}");

    let mut set = Command::cargo_bin("hermes").expect("binary exists");
    set.env("HERMES_HOME", home);
    set.args(["config", "set", "model", model.as_str()]);
    let set_output = set.assert().success().get_output().clone();
    let raw = fs::read_to_string(home.join("config.yaml")).expect("read config");
    assert!(raw.contains(user) && raw.contains(password) && raw.contains(query));

    let mut get = Command::cargo_bin("hermes").expect("binary exists");
    get.env("HERMES_HOME", home);
    get.args(["config", "get", "model"]);
    let get_output = get.assert().success().get_output().clone();

    let mut show = Command::cargo_bin("hermes").expect("binary exists");
    show.env("HERMES_HOME", home);
    show.args(["config", "show"]);
    let show_output = show.assert().success().get_output().clone();

    for (surface, output) in [
        ("set", set_output),
        ("get", get_output),
        ("show", show_output),
    ] {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        for secret in [user, password, query] {
            assert!(
                !stdout.contains(secret) && !stderr.contains(secret),
                "{surface} leaked {secret}: stdout={stdout:?} stderr={stderr:?}"
            );
        }
    }
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
    assert_eq!(text.trim(), "[REDACTED]");
}

#[test]
fn e2e_cli_model_switch_warns_on_stale_auxiliary_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();

    let mut set_compression_provider = Command::cargo_bin("hermes").expect("binary exists");
    set_compression_provider.env("HERMES_HOME", home);
    set_compression_provider.args(["config", "set", "auxiliary.compression.provider", "nous"]);
    set_compression_provider.assert().success();

    let mut set_compression_model = Command::cargo_bin("hermes").expect("binary exists");
    set_compression_model.env("HERMES_HOME", home);
    set_compression_model.args(["config", "set", "auxiliary.compression.model", "hermes-4"]);
    set_compression_model.assert().success();

    let mut set_vision_auto = Command::cargo_bin("hermes").expect("binary exists");
    set_vision_auto.env("HERMES_HOME", home);
    set_vision_auto.args(["config", "set", "auxiliary.vision.provider", "auto"]);
    set_vision_auto.assert().success();

    let mut switch = Command::cargo_bin("hermes").expect("binary exists");
    switch.env("HERMES_HOME", home);
    switch.args(["model", "openrouter:anthropic/claude-opus-4.8"]);
    let out = switch.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("Model switched to: openrouter:anthropic/claude-opus-4.8"),
        "expected model switch output, got: {text:?}"
    );
    assert!(
        text.contains(
            "Warning: 1 auxiliary task (compression=nous/hermes-4) still run on providers other than main 'openrouter'"
        ),
        "expected stale auxiliary warning, got: {text:?}"
    );
    assert!(
        !text.contains("vision"),
        "auto auxiliary provider should not be reported stale: {text:?}"
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
        vec!["systems", "a2a-availability", "show", "--json"],
        vec!["systems", "mcp", "diagnostic", "--json"],
        vec!["systems", "acp", "diagnostic", "--json"],
        vec!["systems", "providers", "--json"],
        vec!["systems", "handoff", "template", "--json"],
        vec!["systems", "provenance", "--json"],
    ] {
        let expects_a2a_availability = args == ["systems", "a2a-availability", "show", "--json"];
        let mut cmd = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
        cmd.env("HERMES_HOME", dir.path());
        cmd.args(&args);
        let out = cmd.assert().success().get_output().stdout.clone();
        let text = std::str::from_utf8(&out).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|err| panic!("json parse failed: {err}; {text}"));
        assert!(
            value.get("kind").is_some() || value.get("name").is_some(),
            "expected report/card json, got {value}"
        );
        if expects_a2a_availability {
            assert_eq!(
                value.get("kind").and_then(serde_json::Value::as_str),
                Some("hermes.a2a.availability")
            );
            assert_eq!(
                value.get("available").and_then(serde_json::Value::as_bool),
                Some(false)
            );
            assert_eq!(
                value
                    .get("production_complete")
                    .and_then(serde_json::Value::as_bool),
                Some(false)
            );
        }
    }
}

#[test]
fn e2e_cli_systems_replay_marks_legacy_trace_diagnostic_only() {
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
    assert_eq!(
        report.get("kind").and_then(serde_json::Value::as_str),
        Some("hermes.run-export.report")
    );
    assert_eq!(
        report
            .get("assessed_export_count")
            .and_then(serde_json::Value::as_u64),
        Some(0)
    );
    assert_eq!(
        report
            .pointer("/diagnostic_only/log_count")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
    assert_eq!(
        report
            .get("evidence_complete")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        report.get("passed").and_then(serde_json::Value::as_bool),
        Some(false)
    );
}
