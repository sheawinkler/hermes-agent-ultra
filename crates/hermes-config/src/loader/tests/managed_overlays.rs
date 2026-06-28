#[test]
fn managed_scope_dir_ignores_missing_override() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
fn load_config_managed_overlay_is_fail_open_on_malformed_yaml() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "max_turns: 17\n").unwrap();
    std::fs::write(managed.path().join("config.yaml"), ":\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).unwrap();

    assert_eq!(cfg.max_turns, 17);

    clear_managed_scope_test_env();
}

#[test]
fn load_config_managed_overlay_is_fail_open_on_invalid_values() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "max_turns: 17\n").unwrap();
    std::fs::write(managed.path().join("config.yaml"), "max_turns: 0\n").unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).unwrap();

    assert_eq!(cfg.max_turns, 17);

    clear_managed_scope_test_env();
}

#[test]
fn load_config_ignore_user_config_still_honors_managed_scope() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
fn load_effective_config_yaml_value_applies_managed_raw_overlay() {
    use tempfile::tempdir;

    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_managed_scope_test_env();
    let home = tempdir().unwrap();
    let managed = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        r#"
cron:
  provider: user
  chronos:
    portal_url: https://user.example
    callback_url: https://agent.example
"#,
    )
    .unwrap();
    std::fs::write(
        managed.path().join("config.yaml"),
        r#"
cron:
  provider: chronos
  chronos:
    portal_url: https://managed.example
"#,
    )
    .unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());

    let root = load_effective_config_yaml_value(&home.path().join("config.yaml")).unwrap();

    assert_eq!(root["cron"]["provider"], "chronos");
    assert_eq!(
        root["cron"]["chronos"]["portal_url"],
        "https://managed.example"
    );
    assert_eq!(
        root["cron"]["chronos"]["callback_url"],
        "https://agent.example"
    );

    clear_managed_scope_test_env();
}

#[test]
fn load_config_bridges_discord_allow_from_alias_to_allowed_users() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
