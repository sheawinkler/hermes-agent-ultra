use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_terminal_env_bridge_vars() {
    for (_, env_key) in terminal_config_env_bridge_pairs() {
        // SAFETY: tests serialize env mutation with ENV_LOCK.
        unsafe { std::env::remove_var(env_key) };
    }
    // SAFETY: tests serialize env mutation with ENV_LOCK.
    unsafe { std::env::remove_var("TERMINAL_BACKEND") };
}

fn clear_web_env_bridge_vars() {
    for (_, env_key) in web_config_env_bridge_pairs() {
        // SAFETY: tests serialize env mutation with ENV_LOCK.
        unsafe { std::env::remove_var(env_key) };
    }
}

fn clear_display_env_bridge_vars() {
    for (_, env_key) in display_config_env_bridge_pairs() {
        // SAFETY: tests serialize env mutation with ENV_LOCK.
        unsafe { std::env::remove_var(env_key) };
    }
}

fn remove_test_env(name: &str) {
    // SAFETY: tests serialize env mutation with ENV_LOCK.
    unsafe { std::env::remove_var(name) };
}

fn set_test_env(name: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: tests serialize env mutation with ENV_LOCK.
    unsafe { std::env::set_var(name, value) };
}

fn clear_managed_scope_test_env() {
    for name in [
        "HERMES_HOME",
        "HERMES_MANAGED_DIR",
        "HERMES_IGNORE_USER_CONFIG",
        "HERMES_MODEL",
        "HERMES_MAX_TURNS",
        "HERMES_WEB_BACKEND",
        "HERMES_WEB_SEARCH_BACKEND",
        "HERMES_GATEWAY_BUSY_INPUT_MODE",
        "HERMES_GATEWAY_BUSY_ACK_ENABLED",
    ] {
        remove_test_env(name);
    }
}

#[test]
fn validate_valid_config() {
    let config = GatewayConfig::default();
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_zero_max_turns() {
    let mut config = GatewayConfig::default();
    config.max_turns = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_zero_terminal_timeout() {
    let mut config = GatewayConfig::default();
    config.terminal.timeout = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_empty_api_key() {
    let mut config = GatewayConfig::default();
    let mut providers = HashMap::new();
    providers.insert(
        "test".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("".into()),
            ..Default::default()
        },
    );
    config.llm_providers = providers;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_whitespace_api_key() {
    let mut config = GatewayConfig::default();
    let mut providers = HashMap::new();
    providers.insert(
        "test".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("   ".into()),
            ..Default::default()
        },
    );
    config.llm_providers = providers;
    assert!(validate_config(&config).is_err());
}

#[test]
fn normalize_provider_secrets_removes_empty_provider_entries() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "minimax".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("".into()),
            ..Default::default()
        },
    );
    config.llm_providers.insert(
        "openrouter".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("   ".into()),
            oauth_token_url: Some("  ".into()),
            ..Default::default()
        },
    );
    config.llm_providers.insert(
        "nous".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("tok-abc".into()),
            ..Default::default()
        },
    );
    normalize_provider_secrets(&mut config);
    assert_eq!(config.llm_providers.len(), 1);
    assert_eq!(
        config
            .llm_providers
            .get("nous")
            .and_then(|cfg| cfg.api_key.as_deref()),
        Some("tok-abc")
    );
}

#[test]
fn normalize_provider_secrets_keeps_timeout_only_provider_entries() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "anthropic".into(),
        crate::config::LlmProviderConfig {
            request_timeout_seconds: Some(45.0),
            ..Default::default()
        },
    );

    normalize_provider_secrets(&mut config);

    assert_eq!(
        config
            .llm_providers
            .get("anthropic")
            .and_then(|cfg| cfg.request_timeout_seconds),
        Some(45.0)
    );
}

#[test]
fn normalize_provider_secrets_keeps_models_only_provider_entries() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "qianfan-coding".into(),
        crate::config::LlmProviderConfig {
            models: vec!["kimi-k2.5".into(), "glm-5".into()],
            discover_models: false,
            ..Default::default()
        },
    );

    normalize_provider_secrets(&mut config);

    let provider = config
        .llm_providers
        .get("qianfan-coding")
        .expect("models-only provider should remain");
    assert_eq!(provider.models, vec!["kimi-k2.5", "glm-5"]);
    assert!(!provider.discover_models);
}

#[test]
fn validate_llm_provider_request_timeout_seconds() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "anthropic".into(),
        crate::config::LlmProviderConfig {
            request_timeout_seconds: Some(45.0),
            ..Default::default()
        },
    );
    assert!(validate_config(&config).is_ok());

    config
        .llm_providers
        .get_mut("anthropic")
        .expect("provider")
        .request_timeout_seconds = Some(0.0);
    let err = validate_config(&config).unwrap_err().to_string();
    assert!(err.contains("request_timeout_seconds"));
}

#[test]
fn env_overrides_model() {
    let mut config = GatewayConfig::default();
    // Simulate env var (we can't easily set env vars in tests, so test the logic directly)
    config.model = Some("env-model".into());
    assert_eq!(config.model.as_deref(), Some("env-model"));
}

#[test]
fn atomic_json_write_roundtrips_and_cleans_temp() {
    use serde_json::json;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("data.json");
    atomic_json_write(&path, &json!({"key": "value", "nested": {"a": 1}})).unwrap();

    let loaded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded, json!({"key": "value", "nested": {"a": 1}}));
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".tmp.")));
}

#[test]
fn cross_device_rename_detection_matches_platform_error_code() {
    #[cfg(unix)]
    assert!(is_cross_device_rename_error(
        &std::io::Error::from_raw_os_error(18)
    ));
    #[cfg(windows)]
    assert!(is_cross_device_rename_error(
        &std::io::Error::from_raw_os_error(17)
    ));
    assert!(!is_cross_device_rename_error(&std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "ordinary rename failure",
    )));
}

#[test]
fn atomic_yaml_write_appends_extra_content() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("data.yaml");
    let value: serde_yaml::Value = serde_yaml::from_str("key: value\n").unwrap();
    atomic_yaml_write(&path, &value, Some("\n# comment\n")).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("key: value"));
    assert!(text.contains("# comment"));
}

#[test]
fn atomic_yaml_write_indents_mapping_child_sequences() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let value: serde_yaml::Value = serde_yaml::from_str(
        r#"
custom_providers:
  - name: provider-a
    base_url: https://a.example.com
fallback_providers:
  - backup-a
  - backup-b
"#,
    )
    .unwrap();

    atomic_yaml_write(&path, &value, None).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("custom_providers:\n  - "));
    assert!(text.contains("fallback_providers:\n  - backup-a"));
    serde_yaml::from_str::<serde_yaml::Value>(&text).expect("normalized yaml parses");
}

#[test]
fn save_config_yaml_preserves_utf8_personality_and_prompt() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let mut config = GatewayConfig {
        personality: Some("mentor 🧠".to_string()),
        system_prompt: Some("Answer with precision; preserve emoji like 🚀 and café.".to_string()),
        ..GatewayConfig::default()
    };
    config.home_dir = Some("/tmp/should-not-persist".to_string());

    save_config_yaml(&path, &config).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let text = std::str::from_utf8(&bytes).expect("config.yaml must be valid UTF-8");
    assert!(text.contains("mentor 🧠"));
    assert!(text.contains("🚀"));
    assert!(text.contains("café"));
    assert!(!text.contains("should-not-persist"));

    let loaded = load_from_yaml(&path).unwrap();
    assert_eq!(loaded.personality.as_deref(), Some("mentor 🧠"));
    assert_eq!(
        loaded.system_prompt.as_deref(),
        Some("Answer with precision; preserve emoji like 🚀 and café.")
    );
}

#[test]
fn load_from_yaml_expands_env_refs_but_user_config_load_preserves_templates() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  openai:
    api_key: ${TEST_OPENAI_KEY}
    base_url: https://${TEST_OPENAI_HOST}/v1
"#,
    )
    .unwrap();

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("TEST_OPENAI_KEY", "sk-test");
        std::env::set_var("TEST_OPENAI_HOST", "api.example.test");
    }

    let runtime = load_from_yaml(&path).unwrap();
    let openai = runtime.llm_providers.get("openai").unwrap();
    assert_eq!(openai.api_key.as_deref(), Some("sk-test"));
    assert_eq!(
        openai.base_url.as_deref(),
        Some("https://api.example.test/v1")
    );

    let editable = load_user_config_file(&path).unwrap();
    let editable_openai = editable.llm_providers.get("openai").unwrap();
    assert_eq!(
        editable_openai.api_key.as_deref(),
        Some("${TEST_OPENAI_KEY}")
    );
    assert_eq!(
        editable_openai.base_url.as_deref(),
        Some("https://${TEST_OPENAI_HOST}/v1")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("TEST_OPENAI_KEY");
        std::env::remove_var("TEST_OPENAI_HOST");
    }
}

#[test]
fn load_from_yaml_keeps_unresolved_env_refs_verbatim() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  openai:
    api_key: ${MISSING_OPENAI_KEY_FOR_TEST}
"#,
    )
    .unwrap();

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("MISSING_OPENAI_KEY_FOR_TEST") };
    let runtime = load_from_yaml(&path).unwrap();
    assert_eq!(
        runtime
            .llm_providers
            .get("openai")
            .and_then(|provider| provider.api_key.as_deref()),
        Some("${MISSING_OPENAI_KEY_FOR_TEST}")
    );
}

#[test]
fn load_from_yaml_tolerates_null_top_level_sections() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
model: null
personality: null
tools: null
platforms: null
platform_toolsets: null
llm_providers: null
display: null
terminal: null
web: null
sessions: null
agent: null
personalities: null
"#,
    )
    .unwrap();

    let cfg = load_from_yaml(&path).unwrap();
    let default = GatewayConfig::default();
    assert_eq!(cfg.model, default.model);
    assert_eq!(cfg.personality, default.personality);
    assert_eq!(cfg.tools, default.tools);
    assert_eq!(cfg.platforms, default.platforms);
    assert_eq!(cfg.platform_toolsets, default.platform_toolsets);
    assert_eq!(cfg.llm_providers, default.llm_providers);
    assert_eq!(cfg.display, default.display);
    assert_eq!(cfg.terminal, default.terminal);
    assert_eq!(cfg.web, default.web);
    assert_eq!(cfg.sessions, default.sessions);
    assert_eq!(cfg.agent, default.agent);
}

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
"#,
    )
    .unwrap();
    unsafe { std::env::set_var("HERMES_GATEWAY_BUSY_INPUT_MODE", "queue") };

    let cfg =
        load_config(Some(dir.path().to_string_lossy().as_ref())).expect("load display config");

    assert_eq!(cfg.display.normalized_busy_input_mode(), "queue");
    assert!(!cfg.display.busy_ack_enabled());
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

#[test]
fn set_user_config_value_preserves_list_siblings_for_indexed_paths() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
custom_providers:
- name: provider-a
  api_key: old-a
  base_url: https://a.example.com
- name: provider-b
  api_key: old-b
  base_url: https://b.example.com
"#,
    )
    .unwrap();

    set_user_config_value(dir.path(), "custom_providers.0.api_key", "new-a").unwrap();

    let reloaded: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let providers = reloaded
        .get("custom_providers")
        .and_then(|v| v.as_sequence())
        .unwrap();
    assert_eq!(providers.len(), 2);
    assert_eq!(
        providers[0].get("api_key").and_then(|v| v.as_str()),
        Some("new-a")
    );
    assert_eq!(
        providers[0].get("base_url").and_then(|v| v.as_str()),
        Some("https://a.example.com")
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("custom_providers:\n  - "));
    assert_eq!(
        providers[1].get("api_key").and_then(|v| v.as_str()),
        Some("old-b")
    );
}

#[test]
fn apply_patch_save_load_roundtrip() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let mut c = GatewayConfig::default();
    apply_user_config_patch(&mut c, "model", "openai:gpt-4o-mini").unwrap();
    apply_user_config_patch(&mut c, "max_turns", "15").unwrap();
    save_config_yaml(&path, &c).unwrap();
    let loaded = load_user_config_file(&path).unwrap();
    assert_eq!(loaded.model.as_deref(), Some("openai:gpt-4o-mini"));
    assert_eq!(loaded.max_turns, 15);
}

#[cfg(unix)]
#[test]
fn save_config_yaml_writes_owner_only_file_when_chmod_enabled() {
    if !config_chmod_enabled() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    save_config_yaml(&path, &GatewayConfig::default()).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn set_user_config_value_writes_owner_only_config_when_chmod_enabled() {
    if !config_chmod_enabled() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    set_user_config_value(dir.path(), "max_turns", "42").unwrap();

    let mode = std::fs::metadata(dir.path().join("config.yaml"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn load_user_config_file_parses_agent_api_max_retries_aliases() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let snake_path = dir.path().join("snake.yaml");
    std::fs::write(
        &snake_path,
        r#"
agent:
  api_max_retries: 6
"#,
    )
    .unwrap();
    let snake = load_user_config_file(&snake_path).unwrap();
    assert_eq!(snake.agent.api_max_retries, Some(6));

    let camel_path = dir.path().join("camel.yaml");
    std::fs::write(
        &camel_path,
        r#"
agent:
  apiMaxRetries: 8
"#,
    )
    .unwrap();
    let camel = load_user_config_file(&camel_path).unwrap();
    assert_eq!(camel.agent.api_max_retries, Some(8));
}

#[test]
fn prefill_messages_file_resolution_prefers_env_then_top_level_then_legacy_agent_key() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("HERMES_PREFILL_MESSAGES_FILE") };
    let mut cfg = GatewayConfig::default();
    cfg.agent.prefill_messages_file = Some("legacy.json".to_string());
    assert_eq!(
        resolve_prefill_messages_file(&cfg).as_deref(),
        Some("legacy.json")
    );

    cfg.prefill_messages_file = Some("top.json".to_string());
    assert_eq!(
        resolve_prefill_messages_file(&cfg).as_deref(),
        Some("top.json")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::set_var("HERMES_PREFILL_MESSAGES_FILE", "env.json") };
    assert_eq!(
        resolve_prefill_messages_file(&cfg).as_deref(),
        Some("env.json")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("HERMES_PREFILL_MESSAGES_FILE") };
}

#[test]
fn load_config_accepts_canonical_and_legacy_prefill_messages_file_keys() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("HERMES_PREFILL_MESSAGES_FILE") };

    let dir = tempdir().unwrap();
    let top_path = dir.path().join("top.yaml");
    std::fs::write(&top_path, "prefill_messages_file: prefill-top.json\n").unwrap();
    let top = load_user_config_file(&top_path).unwrap();
    assert_eq!(
        top.prefill_messages_file.as_deref(),
        Some("prefill-top.json")
    );

    let legacy_path = dir.path().join("legacy.yaml");
    std::fs::write(
        &legacy_path,
        "agent:\n  prefill_messages_file: prefill-legacy.json\n",
    )
    .unwrap();
    let legacy = load_user_config_file(&legacy_path).unwrap();
    assert_eq!(
        legacy.agent.prefill_messages_file.as_deref(),
        Some("prefill-legacy.json")
    );
    assert_eq!(
        resolve_prefill_messages_file(&legacy).as_deref(),
        Some("prefill-legacy.json")
    );
}

#[test]
fn load_prefill_messages_resolves_relative_json_under_config_home() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("HERMES_PREFILL_MESSAGES_FILE") };

    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("prefill.json"),
        r#"[{"role":"system","content":"few-shot system"},{"role":"user","content":"example"}]"#,
    )
    .unwrap();
    let cfg = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().to_string()),
        prefill_messages_file: Some("prefill.json".to_string()),
        ..GatewayConfig::default()
    };

    let messages = load_prefill_messages(&cfg);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, hermes_core::MessageRole::System);
    assert_eq!(messages[0].content.as_deref(), Some("few-shot system"));
    assert_eq!(messages[1].role, hermes_core::MessageRole::User);
    assert_eq!(messages[1].content.as_deref(), Some("example"));
}

#[test]
fn load_user_config_file_parses_auxiliary_task_overrides() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
auxiliary:
  vision:
    provider: openrouter
    model: google/gemini-2.5-flash
    base_url: http://localhost:1234/v1
    api_key: local-key
    timeout: 120
    download_timeout: 30
  web_extract:
    provider: auto
    model: custom-llm
"#,
    )
    .unwrap();

    let loaded = load_user_config_file(&path).unwrap();
    let vision = loaded.auxiliary.get("vision").expect("vision config");
    assert_eq!(vision.provider, "openrouter");
    assert_eq!(vision.model, "google/gemini-2.5-flash");
    assert_eq!(vision.base_url, "http://localhost:1234/v1");
    assert_eq!(vision.api_key, "local-key");
    assert_eq!(vision.timeout, Some(120));
    assert_eq!(vision.download_timeout, Some(30));
    let web_extract = loaded
        .auxiliary
        .get("web_extract")
        .expect("web extract config");
    assert_eq!(web_extract.provider, "auto");
    assert_eq!(web_extract.model, "custom-llm");
}

#[test]
fn load_user_config_file_parses_llm_provider_api_mode() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  codex:
    base_url: https://gateway.example.com/v1
    api_key_env: CODEX_KEY
    api_mode: codex_responses
"#,
    )
    .unwrap();

    let loaded = load_user_config_file(&path).unwrap();
    let provider = loaded.llm_providers.get("codex").expect("codex provider");
    assert_eq!(provider.api_mode.as_deref(), Some("codex_responses"));
}

#[test]
fn load_user_config_file_parses_llm_provider_max_output_tokens_alias() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  custom:
    base_url: https://gateway.example.com/v1
    max_output_tokens: 4096
"#,
    )
    .unwrap();

    let loaded = load_user_config_file(&path).unwrap();
    let provider = loaded.llm_providers.get("custom").expect("custom provider");
    assert_eq!(provider.max_tokens, Some(4096));
}

#[test]
fn load_user_config_file_parses_provider_models_and_discovery_flag() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  qianfan-coding:
    base_url: https://qianfan.baidubce.com/v2/coding
    discover_models: "false"
    models:
      kimi-k2.5:
        context_length: 128000
      glm-5:
        context_length: 128000
"#,
    )
    .unwrap();

    let loaded = load_user_config_file(&path).unwrap();
    let provider = loaded
        .llm_providers
        .get("qianfan-coding")
        .expect("provider");
    assert!(!provider.discover_models);
    assert_eq!(provider.models, vec!["kimi-k2.5", "glm-5"]);
}

#[test]
fn load_user_config_file_rejects_unknown_llm_provider_api_mode() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  custom:
    base_url: https://gateway.example.com/v1
    api_mode: random_wire_shape
"#,
    )
    .unwrap();

    let err = load_user_config_file(&path).unwrap_err().to_string();
    assert!(err.contains("llm_providers.custom.api_mode"));
}

#[test]
fn apply_patch_dotted_llm_proxy_budget() {
    let mut c = GatewayConfig::default();
    apply_user_config_patch(&mut c, "llm.openai.api_key", "sk-test").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.base_url", "https://api.openai.com/v1").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.api_mode", "codex-responses").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.models", "gpt-4o,gpt-4o-mini").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.discover_models", "false").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.max_output_tokens", "8192").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.command", "copilot-language-server").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.args", "--stdio,--model,gpt-4o-mini").unwrap();
    apply_user_config_patch(&mut c, "llm.openai.request_timeout_seconds", "45.5").unwrap();
    apply_user_config_patch(&mut c, "proxy.http", "http://127.0.0.1:8080").unwrap();
    apply_user_config_patch(&mut c, "budget.max_result_size_chars", "500").unwrap();
    apply_user_config_patch(&mut c, "sessions.auto_prune", "true").unwrap();
    apply_user_config_patch(&mut c, "sessions.retention_days", "30").unwrap();
    apply_user_config_patch(&mut c, "sessions.vacuum_after_prune", "false").unwrap();
    apply_user_config_patch(&mut c, "sessions.min_interval_hours", "12").unwrap();
    assert_eq!(
        c.llm_providers.get("openai").unwrap().api_key.as_deref(),
        Some("sk-test")
    );
    assert_eq!(
        c.llm_providers.get("openai").unwrap().base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(
        c.llm_providers.get("openai").unwrap().api_mode.as_deref(),
        Some("codex_responses")
    );
    assert_eq!(
        c.llm_providers.get("openai").unwrap().max_tokens,
        Some(8192)
    );
    assert_eq!(
        c.llm_providers.get("openai").unwrap().models,
        vec!["gpt-4o", "gpt-4o-mini"]
    );
    assert!(!c.llm_providers.get("openai").unwrap().discover_models);
    assert_eq!(
        c.llm_providers.get("openai").unwrap().command.as_deref(),
        Some("copilot-language-server")
    );
    assert_eq!(
        c.llm_providers.get("openai").unwrap().args,
        vec![
            "--stdio".to_string(),
            "--model".to_string(),
            "gpt-4o-mini".to_string()
        ]
    );
    assert_eq!(
        c.llm_providers
            .get("openai")
            .unwrap()
            .request_timeout_seconds,
        Some(45.5)
    );
    assert_eq!(
        c.proxy.as_ref().unwrap().http_proxy.as_deref(),
        Some("http://127.0.0.1:8080")
    );
    assert_eq!(c.budget.max_result_size_chars, 500);
    assert!(c.sessions.auto_prune);
    assert_eq!(c.sessions.retention_days, 30);
    assert!(!c.sessions.vacuum_after_prune);
    assert_eq!(c.sessions.min_interval_hours, 12);
    assert!(user_config_field_display(&c, "llm.openai.api_key")
        .unwrap()
        .starts_with("***"));
    assert_eq!(
        user_config_field_display(&c, "llm.openai.command").unwrap(),
        "copilot-language-server"
    );
    assert_eq!(
        user_config_field_display(&c, "llm.openai.api_mode").unwrap(),
        "codex_responses"
    );
    assert_eq!(
        user_config_field_display(&c, "llm.openai.max_tokens").unwrap(),
        "8192"
    );
    assert_eq!(
        user_config_field_display(&c, "llm.openai.max_output_tokens").unwrap(),
        "8192"
    );
    assert_eq!(
        user_config_field_display(&c, "llm.openai.models").unwrap(),
        "gpt-4o,gpt-4o-mini"
    );
    assert_eq!(
        user_config_field_display(&c, "llm.openai.discover_models").unwrap(),
        "false"
    );
    assert_eq!(
        user_config_field_display(&c, "llm.openai.args").unwrap(),
        "--stdio,--model,gpt-4o-mini"
    );
    assert_eq!(
        user_config_field_display(&c, "llm.openai.request_timeout_seconds").unwrap(),
        "45.5"
    );
    assert_eq!(
        user_config_field_display(&c, "sessions.auto_prune").unwrap(),
        "true"
    );
    assert_eq!(
        user_config_field_display(&c, "sessions.retention_days").unwrap(),
        "30"
    );
    assert!(apply_user_config_patch(&mut c, "llm.openai.request_timeout_seconds", "0").is_err());
    assert!(apply_user_config_patch(&mut c, "llm.openai.request_timeout_seconds", "fast").is_err());
    assert!(apply_user_config_patch(&mut c, "llm.openai.max_tokens", "0").is_err());
    assert!(apply_user_config_patch(&mut c, "llm.openai.discover_models", "maybe").is_err());
}

#[test]
fn apply_patch_dotted_kanban_dispatch_gate() {
    let mut c = GatewayConfig::default();
    assert!(c.kanban.dispatch_in_gateway);

    apply_user_config_patch(&mut c, "kanban.dispatch_in_gateway", "false").unwrap();

    assert!(!c.kanban.dispatch_in_gateway);
    assert_eq!(
        user_config_field_display(&c, "kanban.dispatch_in_gateway").unwrap(),
        "false"
    );
    assert!(apply_user_config_patch(&mut c, "kanban.dispatch_in_gateway", "maybe").is_err());
}

#[test]
fn apply_patch_dotted_agent_api_max_retries() {
    let mut c = GatewayConfig::default();
    assert_eq!(c.agent.api_max_retries, None);

    apply_user_config_patch(&mut c, "agent.api_max_retries", "7").unwrap();

    assert_eq!(c.agent.api_max_retries, Some(7));
    assert_eq!(
        user_config_field_display(&c, "agent.api_max_retries").unwrap(),
        "7"
    );
    assert!(apply_user_config_patch(&mut c, "agent.api_max_retries", "nope").is_err());
}

#[test]
fn apply_patch_dotted_delegation_provider_model_runtime_values() {
    let mut c = GatewayConfig::default();

    apply_user_config_patch(&mut c, "delegation.model", "google/gemini-3-flash-preview").unwrap();
    apply_user_config_patch(&mut c, "delegation.provider", "openrouter").unwrap();
    apply_user_config_patch(&mut c, "delegation.base_url", "http://localhost:1234/v1").unwrap();
    apply_user_config_patch(&mut c, "delegation.api_key", "local-key").unwrap();
    apply_user_config_patch(&mut c, "delegation.max_spawn_depth", "9").unwrap();

    assert_eq!(
        c.delegation.model.as_deref(),
        Some("google/gemini-3-flash-preview")
    );
    assert_eq!(c.delegation.provider.as_deref(), Some("openrouter"));
    assert_eq!(
        c.delegation.base_url.as_deref(),
        Some("http://localhost:1234/v1")
    );
    assert_eq!(c.delegation.api_key.as_deref(), Some("local-key"));
    assert_eq!(c.delegation.max_spawn_depth, Some(9));
    assert_eq!(
        user_config_field_display(&c, "delegation.model").unwrap(),
        "google/gemini-3-flash-preview"
    );
    assert_eq!(
        user_config_field_display(&c, "delegation.provider").unwrap(),
        "openrouter"
    );
    assert_eq!(
        user_config_field_display(&c, "delegation.base_url").unwrap(),
        "http://localhost:1234/v1"
    );
    assert!(user_config_field_display(&c, "delegation.api_key")
        .unwrap()
        .starts_with("***"));
    assert_eq!(
        user_config_field_display(&c, "delegation.max_spawn_depth").unwrap(),
        "9"
    );
    assert!(apply_user_config_patch(&mut c, "delegation.max_spawn_depth", "deep").is_err());
}

#[test]
fn apply_patch_dotted_auxiliary_values() {
    let mut c = GatewayConfig::default();
    apply_user_config_patch(&mut c, "auxiliary.vision.provider", "openrouter").unwrap();
    apply_user_config_patch(&mut c, "auxiliary.vision.model", "google/gemini-2.5-flash").unwrap();
    apply_user_config_patch(
        &mut c,
        "auxiliary.vision.base_url",
        "http://localhost:1234/v1",
    )
    .unwrap();
    apply_user_config_patch(&mut c, "auxiliary.vision.api_key", "local-key").unwrap();
    apply_user_config_patch(&mut c, "auxiliary.vision.timeout", "120").unwrap();
    apply_user_config_patch(&mut c, "auxiliary.vision.download_timeout", "30").unwrap();

    let vision = c.auxiliary.get("vision").expect("vision config");
    assert_eq!(vision.provider, "openrouter");
    assert_eq!(vision.model, "google/gemini-2.5-flash");
    assert_eq!(vision.base_url, "http://localhost:1234/v1");
    assert_eq!(vision.api_key, "local-key");
    assert_eq!(vision.timeout, Some(120));
    assert_eq!(vision.download_timeout, Some(30));
    assert_eq!(
        user_config_field_display(&c, "auxiliary.vision.provider").unwrap(),
        "openrouter"
    );
    assert_eq!(
        user_config_field_display(&c, "auxiliary.vision.model").unwrap(),
        "google/gemini-2.5-flash"
    );
    assert_eq!(
        user_config_field_display(&c, "auxiliary.vision.api_key").unwrap(),
        "***-key"
    );
    assert_eq!(
        user_config_field_display(&c, "auxiliary.vision.timeout").unwrap(),
        "120"
    );
}

#[test]
fn sanitize_env_lines_splits_concatenated_assignments() {
    let raw = "TELEGRAM_BOT_TOKEN=12345ANTHROPIC_API_KEY=sk-ant-test\n";
    let sanitized = sanitize_env_lines(raw);
    assert_eq!(
        sanitized,
        "TELEGRAM_BOT_TOKEN=12345\nANTHROPIC_API_KEY=sk-ant-test\n"
    );
}

#[test]
fn load_dotenv_file_sanitizes_project_env_before_loading() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let project = tempdir().expect("project tempdir");
    let project_env = project.path().join(".env");

    std::fs::write(
        &project_env,
        "TELEGRAM_BOT_TOKEN=abc123ANTHROPIC_API_KEY=sk-ant-test\n",
    )
    .expect("write project env");

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("TELEGRAM_BOT_TOKEN", "stale");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    sanitize_env_file_if_needed(&project_env);
    load_dotenv_file(&project_env, true);

    assert_eq!(
        std::env::var("TELEGRAM_BOT_TOKEN").ok().as_deref(),
        Some("abc123")
    );
    assert_eq!(
        std::env::var("ANTHROPIC_API_KEY").ok().as_deref(),
        Some("sk-ant-test")
    );
    let rewritten = std::fs::read_to_string(&project_env).expect("read sanitized env");
    assert!(rewritten.contains("\nANTHROPIC_API_KEY="));

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("TELEGRAM_BOT_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
}

#[test]
fn project_env_only_fills_missing_when_user_env_loaded() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir().expect("tempdir");
    let user_env = dir.path().join("user.env");
    let project_env = dir.path().join("project.env");
    std::fs::write(&user_env, "OPENAI_API_KEY=user-key\n").expect("write user env");
    std::fs::write(
        &project_env,
        "OPENAI_API_KEY=project-key\nEXA_API_KEY=exa-key\n",
    )
    .expect("write project env");

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("EXA_API_KEY");
    }

    load_dotenv_file(&user_env, true);
    load_dotenv_file(&project_env, false);

    assert_eq!(
        std::env::var("OPENAI_API_KEY").ok().as_deref(),
        Some("user-key")
    );
    assert_eq!(
        std::env::var("EXA_API_KEY").ok().as_deref(),
        Some("exa-key")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("EXA_API_KEY");
    }
}

#[test]
fn apply_env_overrides_ignores_empty_provider_keys() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("OPENROUTER_API_KEY", "");
        std::env::set_var("MINIMAX_API_KEY", "   ");
        std::env::remove_var("NOUS_API_KEY");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);

    assert!(
        !cfg.llm_providers.contains_key("openrouter"),
        "empty OPENROUTER_API_KEY should not create provider entry"
    );
    assert!(
        !cfg.llm_providers.contains_key("minimax"),
        "empty MINIMAX_API_KEY should not create provider entry"
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
        std::env::remove_var("MINIMAX_API_KEY");
    }
}

#[test]
fn apply_env_overrides_supports_kanban_dispatch_gate() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe {
        std::env::set_var("HERMES_KANBAN_DISPATCH_IN_GATEWAY", "false");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);
    assert!(!cfg.kanban.dispatch_in_gateway);

    unsafe {
        std::env::remove_var("HERMES_KANBAN_DISPATCH_IN_GATEWAY");
    }
}

#[test]
fn apply_env_overrides_supports_agent_api_max_retries() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe {
        std::env::set_var("HERMES_AGENT_API_MAX_RETRIES", "9");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);
    assert_eq!(cfg.agent.api_max_retries, Some(9));

    unsafe {
        std::env::remove_var("HERMES_AGENT_API_MAX_RETRIES");
    }
}

#[test]
fn apply_env_overrides_openai_falls_back_when_primary_env_is_empty() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("HERMES_OPENAI_API_KEY", "");
        std::env::set_var("OPENAI_API_KEY", "fallback-openai-key");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);

    assert_eq!(
        cfg.llm_providers
            .get("openai")
            .and_then(|p| p.api_key.as_deref()),
        Some("fallback-openai-key")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("HERMES_OPENAI_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
    }
}

#[test]
fn apply_env_overrides_supports_codex_and_qwen_oauth_env_vars() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("HERMES_OPENAI_CODEX_API_KEY", "codex-token");
        std::env::set_var("HERMES_QWEN_OAUTH_API_KEY", "qwen-oauth-token");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);

    assert_eq!(
        cfg.llm_providers
            .get("openai-codex")
            .and_then(|p| p.api_key.as_deref()),
        Some("codex-token")
    );
    assert_eq!(
        cfg.llm_providers
            .get("qwen-oauth")
            .and_then(|p| p.api_key.as_deref()),
        Some("qwen-oauth-token")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("HERMES_OPENAI_CODEX_API_KEY");
        std::env::remove_var("HERMES_QWEN_OAUTH_API_KEY");
    }
}

#[test]
fn apply_env_overrides_supports_copilot_env_var_precedence() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("COPILOT_GITHUB_TOKEN", "copilot-primary");
        std::env::set_var("GITHUB_COPILOT_TOKEN", "legacy-fallback");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);

    assert_eq!(
        cfg.llm_providers
            .get("copilot")
            .and_then(|p| p.api_key.as_deref()),
        Some("copilot-primary")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("COPILOT_GITHUB_TOKEN");
        std::env::remove_var("GITHUB_COPILOT_TOKEN");
    }
}

#[test]
fn apply_env_overrides_supports_direct_provider_env_vars() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe {
        std::env::set_var("ARCEEAI_API_KEY", "arcee-token");
        std::env::set_var("XIAOMI_API_KEY", "xiaomi-token");
        std::env::set_var("TOKENHUB_API_KEY", "tokenhub-token");
        std::env::set_var("TOKENHUB_BASE_URL", "https://tokenhub.example/v1");
        std::env::set_var("LLAMA_CPP_API_KEY", "llama-cpp-token");
        std::env::set_var("LLAMA_CPP_BASE_URL", "http://127.0.0.1:8080/v1");
        std::env::set_var("LMSTUDIO_API_KEY", "lmstudio-token");
        std::env::set_var("LMSTUDIO_BASE_URL", "http://127.0.0.1:1234/v1");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);

    assert_eq!(
        cfg.llm_providers
            .get("arcee")
            .and_then(|p| p.api_key.as_deref()),
        Some("arcee-token")
    );
    assert_eq!(
        cfg.llm_providers
            .get("xiaomi")
            .and_then(|p| p.api_key.as_deref()),
        Some("xiaomi-token")
    );
    let tokenhub = cfg
        .llm_providers
        .get("tencent-tokenhub")
        .expect("tokenhub provider");
    assert_eq!(tokenhub.api_key.as_deref(), Some("tokenhub-token"));
    assert_eq!(
        tokenhub.base_url.as_deref(),
        Some("https://tokenhub.example/v1")
    );
    let llama_cpp = cfg
        .llm_providers
        .get("llama-cpp")
        .expect("llama-cpp provider");
    assert_eq!(llama_cpp.api_key.as_deref(), Some("llama-cpp-token"));
    assert_eq!(
        llama_cpp.base_url.as_deref(),
        Some("http://127.0.0.1:8080/v1")
    );
    let lmstudio = cfg
        .llm_providers
        .get("lmstudio")
        .expect("lmstudio provider");
    assert_eq!(lmstudio.api_key.as_deref(), Some("lmstudio-token"));
    assert_eq!(
        lmstudio.base_url.as_deref(),
        Some("http://127.0.0.1:1234/v1")
    );

    unsafe {
        std::env::remove_var("ARCEEAI_API_KEY");
        std::env::remove_var("XIAOMI_API_KEY");
        std::env::remove_var("TOKENHUB_API_KEY");
        std::env::remove_var("TOKENHUB_BASE_URL");
        std::env::remove_var("LLAMA_CPP_API_KEY");
        std::env::remove_var("LLAMA_CPP_BASE_URL");
        std::env::remove_var("LMSTUDIO_API_KEY");
        std::env::remove_var("LMSTUDIO_BASE_URL");
    }
}

#[test]
fn apply_env_overrides_ignores_generic_github_tokens_for_copilot() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("COPILOT_GITHUB_TOKEN");
        std::env::remove_var("GITHUB_COPILOT_TOKEN");
        std::env::set_var("GH_TOKEN", "generic-gh-token");
        std::env::set_var("GITHUB_TOKEN", "generic-github-token");
    }

    let mut cfg = GatewayConfig::default();
    apply_env_overrides(&mut cfg);

    assert!(
        !cfg.llm_providers.contains_key("copilot"),
        "generic GitHub tokens should not auto-configure the Copilot provider"
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
    }
}

#[test]
fn apply_env_overrides_slack_env_token_enables_platform_by_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe {
        std::env::set_var("SLACK_BOT_TOKEN", "[REDACTED_SLACK_TOKEN]");
    }

    let mut cfg = GatewayConfig::default();
    let slack = cfg
        .platforms
        .entry("slack".to_string())
        .or_insert_with(crate::platform::PlatformConfig::default);
    slack.enabled = false;
    slack.extra.insert(
        "channel_prompts".to_string(),
        serde_json::json!({"C1":"ops"}),
    );

    apply_env_overrides(&mut cfg);

    let slack = cfg.platforms.get("slack").expect("slack config");
    assert!(slack.enabled, "env token should auto-enable slack");
    assert_eq!(slack.token.as_deref(), Some("[REDACTED_SLACK_TOKEN]"));

    unsafe {
        std::env::remove_var("SLACK_BOT_TOKEN");
    }
}

#[test]
fn apply_env_overrides_slack_env_token_respects_explicit_disable_marker() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    unsafe {
        std::env::set_var("SLACK_BOT_TOKEN", "[REDACTED_SLACK_TOKEN]");
    }

    let mut cfg = GatewayConfig::default();
    let slack = cfg
        .platforms
        .entry("slack".to_string())
        .or_insert_with(crate::platform::PlatformConfig::default);
    slack.enabled = false;
    slack
        .extra
        .insert("_enabled_explicit".to_string(), serde_json::json!(true));

    apply_env_overrides(&mut cfg);

    let slack = cfg.platforms.get("slack").expect("slack config");
    assert!(
        !slack.enabled,
        "explicitly disabled slack config must remain disabled"
    );
    assert_eq!(slack.token.as_deref(), Some("[REDACTED_SLACK_TOKEN]"));

    unsafe {
        std::env::remove_var("SLACK_BOT_TOKEN");
    }
}

#[test]
fn apply_env_overrides_respects_ignore_rules_flags() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("HERMES_IGNORE_RULES", "1");
    }
    let mut cfg = GatewayConfig::default();
    cfg.agent.skip_context_files = false;
    apply_env_overrides(&mut cfg);
    assert!(cfg.agent.skip_context_files);
    unsafe {
        std::env::remove_var("HERMES_IGNORE_RULES");
    }
}

#[test]
fn load_config_ignore_user_config_uses_defaults_when_files_exist() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().expect("tempdir");
    let cfg_path = home.path().join("config.yaml");
    std::fs::write(&cfg_path, "max_turns: 777\n").expect("write config");
    unsafe {
        std::env::set_var("HERMES_IGNORE_USER_CONFIG", "1");
    }
    let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).expect("load config");
    assert_eq!(cfg.max_turns, GatewayConfig::default().max_turns);
    unsafe {
        std::env::remove_var("HERMES_IGNORE_USER_CONFIG");
    }
}
