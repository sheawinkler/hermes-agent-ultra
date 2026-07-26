#[test]
fn managed_scope_dir_ignores_missing_override() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let dir = tempdir().unwrap();
    set_test_env("HERMES_MANAGED_DIR", dir.path().join("missing"));

    assert!(managed_scope_dir().is_none());

    clear_managed_scope_test_env();
}

#[test]
fn load_config_applies_managed_overlay_after_env_and_preserves_siblings() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    clear_web_env_bridge_vars();
    clear_display_env_bridge_vars();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        r#"
max_turns: 12
display:
  busy_input_mode: queue
  busy_ack_enabled: false
web:
  backend: user-web
  search_backend: user-search
"#,
    )
    .unwrap();
    std::fs::write(
        managed.path().join("config.yaml"),
        r#"
display:
  busy_input_mode: steer
web:
  backend: managed-web
"#,
    )
    .unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());
    set_test_env("HERMES_WEB_BACKEND", "shell-web");
    set_test_env("HERMES_GATEWAY_BUSY_INPUT_MODE", "interrupt");

    let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).unwrap();

    assert_eq!(cfg.max_turns, 12);
    assert_eq!(cfg.display.normalized_busy_input_mode(), "steer");
    assert!(!cfg.display.busy_ack_enabled());
    assert_eq!(cfg.web.backend, "managed-web");
    assert_eq!(cfg.web.search_backend, "user-search");
    assert_eq!(std::env::var("HERMES_WEB_BACKEND").unwrap(), "managed-web");
    assert_eq!(
        std::env::var("HERMES_GATEWAY_BUSY_INPUT_MODE").unwrap(),
        "steer"
    );

    clear_managed_scope_test_env();
    clear_web_env_bridge_vars();
    clear_display_env_bridge_vars();
}

#[test]
fn load_dotenv_managed_env_overrides_user_dotenv_and_shell() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join(".env"), "HERMES_MODEL=user/env\n").unwrap();
    std::fs::write(managed.path().join(".env"), "HERMES_MODEL=managed/env\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());
    set_test_env("HERMES_MODEL", "shell/env");

    let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).unwrap();

    assert_eq!(cfg.model.as_deref(), Some("managed/env"));
    assert_eq!(std::env::var("HERMES_MODEL").unwrap(), "managed/env");

    clear_managed_scope_test_env();
}

#[test]
fn load_config_managed_overlay_normalizes_root_model_string() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        "model: user/model\nfallback_models:\n  - user/fallback\n",
    )
    .unwrap();
    std::fs::write(managed.path().join("config.yaml"), "model: managed/model\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).unwrap();

    assert_eq!(cfg.model.as_deref(), Some("managed/model"));
    assert_eq!(cfg.fallback_models, vec!["user/fallback".to_string()]);

    clear_managed_scope_test_env();
}

#[test]
fn load_config_rejects_malformed_managed_overlay_fail_closed() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "max_turns: 17\n").unwrap();
    std::fs::write(managed.path().join("config.yaml"), ":\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    let error = load_config(Some(home.path().to_string_lossy().as_ref()))
        .expect_err("malformed managed overlay must block startup");
    assert!(
        error
            .to_string()
            .contains(managed.path().to_string_lossy().as_ref()),
        "managed error must identify its path: {error}"
    );

    clear_managed_scope_test_env();
}

#[test]
fn load_config_rejects_invalid_managed_overlay_fail_closed() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "max_turns: 17\n").unwrap();
    std::fs::write(managed.path().join("config.yaml"), "max_turns: 0\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    let error = load_config(Some(home.path().to_string_lossy().as_ref()))
        .expect_err("invalid managed overlay must block startup");
    let rendered = error.to_string();
    assert!(
        rendered.contains(managed.path().to_string_lossy().as_ref()),
        "managed error must identify its path: {error}"
    );
    assert!(
        rendered.contains("max_turns"),
        "managed validation error must retain its cause: {error}"
    );

    clear_managed_scope_test_env();
}

#[test]
fn load_config_ignore_user_config_still_honors_managed_scope() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "max_turns: 777\n").unwrap();
    std::fs::write(managed.path().join("config.yaml"), "max_turns: 42\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());
    set_test_env("HERMES_IGNORE_USER_CONFIG", "1");

    let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).unwrap();

    assert_eq!(cfg.max_turns, 42);

    clear_managed_scope_test_env();
}

#[test]
fn load_effective_config_yaml_value_fails_closed_for_unknown_managed_overlay_keys() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "model: user/model\n").unwrap();
    std::fs::write(
        managed.path().join("config.yaml"),
        "unknown_managed_key: true\n",
    )
    .unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    assert!(
        load_effective_config_yaml_value(&home.path().join("config.yaml")).is_none(),
        "raw managed consumers must not bypass a rejected managed overlay"
    );

    clear_managed_scope_test_env();
}

#[test]
fn load_config_bridges_discord_allow_from_alias_to_allowed_users() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock();
    unsafe {
        std::env::remove_var("DISCORD_ALLOWED_USERS");
        std::env::remove_var("DISCORD_BOT_TOKEN");
    }
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.yaml"),
        r#"
platforms:
  discord:
    enabled: true
    allow_from:
      - "100"
      - 200
"#,
    )
    .unwrap();

    let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
    let discord = cfg.platforms.get("discord").expect("discord config");
    assert_eq!(discord.allowed_users, vec!["100", "200"]);
    assert!(discord.extra.contains_key("allow_from"));
}

#[test]
fn load_config_bridges_discord_extra_allow_from_and_preserves_env_precedence() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock();
    unsafe {
        std::env::set_var("DISCORD_ALLOWED_USERS", "env-user");
        std::env::remove_var("DISCORD_BOT_TOKEN");
    }
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.yaml"),
        r#"
platforms:
  discord:
    enabled: true
    extra:
      allow_from: cfg-user-1,cfg-user-2
"#,
    )
    .unwrap();

    let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
    let discord = cfg.platforms.get("discord").expect("discord config");
    assert_eq!(discord.allowed_users, vec!["env-user"]);

    unsafe {
        std::env::remove_var("DISCORD_ALLOWED_USERS");
    }
}

#[test]
fn managed_overlay_rejects_unknown_root_and_nested_keys() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "model: user/model\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    for (key_path, contents) in [
        ("unknown_managed_key", "unknown_managed_key: true\n"),
        (
            "terminal.unknown_terminal_key",
            "terminal:\n  unknown_terminal_key: true\n",
        ),
    ] {
        std::fs::write(managed.path().join("config.yaml"), contents).unwrap();
        let error = load_config(Some(home.path().to_string_lossy().as_ref()))
            .expect_err("unknown managed keys must block startup");
        let rendered = error.to_string();
        assert!(
            rendered.contains("unknown managed config key"),
            "expected strict managed unknown-key failure for {key_path}: {error}"
        );
        assert!(
            rendered.contains(key_path),
            "expected unknown key path {key_path}, got {error}"
        );
        assert!(
            rendered.contains(managed.path().to_string_lossy().as_ref()),
            "expected managed source path, got {error}"
        );
    }

    clear_managed_scope_test_env();
}
