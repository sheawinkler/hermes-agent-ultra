#[test]
fn set_user_config_value_routes_secret_keys_to_env() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir().unwrap();
    let result = set_user_config_value(dir.path(), "openai_api_key", "sk-test").unwrap();

    assert!(result.wrote_env());
    assert!(!result.wrote_config());
    assert_eq!(result.env_key.as_deref(), Some("OPENAI_API_KEY"));
    assert!(std::fs::read_to_string(dir.path().join(".env"))
        .unwrap()
        .contains("OPENAI_API_KEY=sk-test"));
    assert!(!dir.path().join("config.yaml").exists());

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("OPENAI_API_KEY") };
}

#[test]
fn set_user_config_value_bridges_terminal_env_keys_and_config() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_terminal_env_bridge_vars();
    let dir = tempdir().unwrap();
    let result =
        set_user_config_value(dir.path(), "terminal.vercel_runtime", "python3.13").unwrap();

    assert!(result.wrote_config());
    assert!(result.wrote_env());
    assert_eq!(result.env_key.as_deref(), Some("TERMINAL_VERCEL_RUNTIME"));
    let config_text = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
    let env_text = std::fs::read_to_string(dir.path().join(".env")).unwrap();
    assert!(config_text.contains("vercel_runtime: python3.13"));
    assert!(env_text.contains("TERMINAL_VERCEL_RUNTIME=python3.13"));

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("TERMINAL_VERCEL_RUNTIME") };
}

#[test]
fn terminal_config_bridge_map_covers_critical_writable_keys() {
    let keys = terminal_config_env_bridge_pairs()
        .iter()
        .map(|(key, _)| *key)
        .collect::<std::collections::HashSet<_>>();
    for key in [
        "backend",
        "docker_run_as_host_user",
        "docker_mount_cwd_to_workspace",
        "docker_env",
        "docker_image",
        "container_cpu",
        "container_memory",
        "container_disk",
        "container_persistent",
        "shell_init_files",
        "auto_source_bashrc",
        "home_mode",
        "vercel_runtime",
        "modal_mode",
    ] {
        assert!(keys.contains(key), "missing terminal bridge key: {key}");
        assert!(
            terminal_config_env_bridge_key(&format!("terminal.{key}")).is_some(),
            "terminal.{key} should map to an env var"
        );
    }
}

#[test]
fn set_user_config_value_bridges_all_terminal_runtime_keys() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_terminal_env_bridge_vars();
    let dir = tempdir().unwrap();
    for (key, value, env_key) in [
        (
            "terminal.docker_run_as_host_user",
            "true",
            "TERMINAL_DOCKER_RUN_AS_HOST_USER",
        ),
        (
            "terminal.docker_mount_cwd_to_workspace",
            "true",
            "TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE",
        ),
        ("terminal.docker_env", "FOO=bar", "TERMINAL_DOCKER_ENV"),
        (
            "terminal.shell_init_files",
            "~/custom.sh",
            "TERMINAL_SHELL_INIT_FILES",
        ),
        (
            "terminal.auto_source_bashrc",
            "false",
            "TERMINAL_AUTO_SOURCE_BASHRC",
        ),
        ("terminal.home_mode", "profile", "TERMINAL_HOME_MODE"),
    ] {
        let result = set_user_config_value(dir.path(), key, value).unwrap();
        assert!(result.wrote_config(), "{key} should write config");
        assert!(result.wrote_env(), "{key} should write env");
        assert_eq!(result.env_key.as_deref(), Some(env_key));
    }
    let env_text = std::fs::read_to_string(dir.path().join(".env")).unwrap();
    assert!(env_text.contains("TERMINAL_DOCKER_RUN_AS_HOST_USER=true"));
    assert!(env_text.contains("TERMINAL_DOCKER_ENV=FOO=bar"));
    assert!(env_text.contains("TERMINAL_AUTO_SOURCE_BASHRC=false"));
    assert!(env_text.contains("TERMINAL_HOME_MODE=profile"));
    clear_terminal_env_bridge_vars();
}

#[test]
fn web_config_bridge_map_covers_runtime_backend_keys() {
    let keys = web_config_env_bridge_pairs()
        .iter()
        .map(|(key, _)| *key)
        .collect::<std::collections::HashSet<_>>();
    for key in [
        "backend",
        "search_backend",
        "extract_backend",
        "crawl_backend",
    ] {
        assert!(keys.contains(key), "missing web bridge key: {key}");
        assert!(
            web_config_env_bridge_key(&format!("web.{key}")).is_some(),
            "web.{key} should map to an env var"
        );
    }
}

#[test]
fn set_user_config_value_bridges_web_backend_keys() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_web_env_bridge_vars();
    let dir = tempdir().unwrap();
    for (key, value, env_key) in [
        ("web.backend", "firecrawl", "HERMES_WEB_BACKEND"),
        (
            "web.search_backend",
            "brave-free",
            "HERMES_WEB_SEARCH_BACKEND",
        ),
        (
            "web.extract_backend",
            "tavily",
            "HERMES_WEB_EXTRACT_BACKEND",
        ),
        ("web.crawl_backend", "tavily", "HERMES_WEB_CRAWL_BACKEND"),
    ] {
        let result = set_user_config_value(dir.path(), key, value).unwrap();
        assert!(result.wrote_config(), "{key} should write config");
        assert!(result.wrote_env(), "{key} should write env");
        assert_eq!(result.env_key.as_deref(), Some(env_key));
    }
    let env_text = std::fs::read_to_string(dir.path().join(".env")).unwrap();
    assert!(env_text.contains("HERMES_WEB_SEARCH_BACKEND=brave-free"));
    clear_web_env_bridge_vars();
}

#[test]
fn display_config_bridge_map_covers_busy_runtime_keys() {
    let keys = display_config_env_bridge_pairs()
        .iter()
        .map(|(key, _)| *key)
        .collect::<std::collections::HashSet<_>>();
    for key in [
        "busy_input_mode",
        "busy_ack_enabled",
        "memory_notifications",
        "friendly_tool_labels",
    ] {
        assert!(keys.contains(key), "missing display bridge key: {key}");
        assert!(
            display_config_env_bridge_key(&format!("display.{key}")).is_some(),
            "display.{key} should map to an env var"
        );
    }
}

#[test]
fn set_user_config_value_bridges_display_runtime_keys() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_display_env_bridge_vars();
    let dir = tempdir().unwrap();
    for (key, value, env_key) in [
        (
            "display.busy_input_mode",
            "steer",
            "HERMES_GATEWAY_BUSY_INPUT_MODE",
        ),
        (
            "display.busy_ack_enabled",
            "false",
            "HERMES_GATEWAY_BUSY_ACK_ENABLED",
        ),
        (
            "display.memory_notifications",
            "false",
            "HERMES_MEMORY_NOTIFICATIONS_ENABLED",
        ),
        (
            "display.friendly_tool_labels",
            "false",
            "HERMES_FRIENDLY_TOOL_LABELS",
        ),
    ] {
        let result = set_user_config_value(dir.path(), key, value).unwrap();
        assert!(result.wrote_config(), "{key} should write config");
        assert!(result.wrote_env(), "{key} should write env");
        assert_eq!(result.env_key.as_deref(), Some(env_key));
    }
    let env_text = std::fs::read_to_string(dir.path().join(".env")).unwrap();
    assert!(env_text.contains("HERMES_GATEWAY_BUSY_INPUT_MODE=steer"));
    assert!(env_text.contains("HERMES_GATEWAY_BUSY_ACK_ENABLED=false"));
    assert!(env_text.contains("HERMES_MEMORY_NOTIFICATIONS_ENABLED=false"));
    assert!(env_text.contains("HERMES_FRIENDLY_TOOL_LABELS=false"));
    clear_display_env_bridge_vars();
}

#[test]
fn load_config_bridges_display_yaml_to_env_without_overriding_existing_env() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_display_env_bridge_vars();
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.yaml"),
        r#"
display:
  busy_input_mode: steer
  busy_ack_enabled: false
  friendly_tool_labels: false
"#,
    )
    .unwrap();
    unsafe { std::env::set_var("HERMES_GATEWAY_BUSY_INPUT_MODE", "queue") };

    let cfg =
        load_config(Some(dir.path().to_string_lossy().as_ref())).expect("load display config");

    assert_eq!(cfg.display.normalized_busy_input_mode(), "queue");
    assert!(!cfg.display.busy_ack_enabled());
    assert!(!cfg.display.friendly_tool_labels_enabled());
    assert_eq!(
        std::env::var("HERMES_GATEWAY_BUSY_INPUT_MODE")
            .ok()
            .as_deref(),
        Some("queue")
    );
    assert_eq!(
        std::env::var("HERMES_GATEWAY_BUSY_ACK_ENABLED")
            .ok()
            .as_deref(),
        Some("false")
    );
    assert_eq!(
        std::env::var("HERMES_FRIENDLY_TOOL_LABELS")
            .ok()
            .as_deref(),
        Some("false")
    );
    clear_display_env_bridge_vars();
}

#[test]
fn load_config_bridges_web_yaml_to_env_without_overriding_existing_env() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_web_env_bridge_vars();
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.yaml"),
        r#"
web:
  backend: firecrawl
  search_backend: searxng
  extract_backend: tavily
  crawl_backend: tavily
"#,
    )
    .unwrap();
    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "brave-free") };

    let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
    assert_eq!(cfg.web.backend, "firecrawl");
    assert_eq!(cfg.web.search_backend, "brave-free");
    assert_eq!(cfg.web.extract_backend, "tavily");
    assert_eq!(std::env::var("HERMES_WEB_BACKEND").unwrap(), "firecrawl");
    assert_eq!(
        std::env::var("HERMES_WEB_SEARCH_BACKEND").unwrap(),
        "brave-free"
    );
    assert_eq!(
        std::env::var("HERMES_WEB_EXTRACT_BACKEND").unwrap(),
        "tavily"
    );
    clear_web_env_bridge_vars();
}

#[test]
fn load_config_bridges_terminal_yaml_to_env_without_overriding_existing_env() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_terminal_env_bridge_vars();
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.yaml"),
        r#"
terminal:
  backend: docker
  docker_image: rust:1.90
  docker_env: FOO=bar
  docker_mount_cwd_to_workspace: true
  shell_init_files: "~/custom.sh"
  auto_source_bashrc: false
"#,
    )
    .unwrap();
    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::set_var("TERMINAL_DOCKER_IMAGE", "already-set") };

    let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
    assert_eq!(cfg.terminal.backend, TerminalBackendType::Docker);
    assert_eq!(cfg.terminal.docker_image.as_deref(), Some("already-set"));
    assert_eq!(std::env::var("TERMINAL_ENV").unwrap(), "docker");
    assert_eq!(
        std::env::var("TERMINAL_DOCKER_IMAGE").unwrap(),
        "already-set"
    );
    assert_eq!(std::env::var("TERMINAL_DOCKER_ENV").unwrap(), "FOO=bar");
    assert_eq!(
        std::env::var("TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE").unwrap(),
        "true"
    );
    assert_eq!(
        std::env::var("TERMINAL_AUTO_SOURCE_BASHRC").unwrap(),
        "false"
    );
    clear_terminal_env_bridge_vars();
}
