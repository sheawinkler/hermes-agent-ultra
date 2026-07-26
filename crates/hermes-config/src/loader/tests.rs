use super::*;
use crate::config::ExecutionModeConfigValue;
use hermes_core::{ExecutionMode, ExecutionModeSource};
use parking_lot::Mutex;
use std::collections::HashMap;

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
fn atomic_replace_file_replaces_existing_destination() {
    use tempfile::tempdir;

    let dir = tempdir().expect("temporary config directory");
    let target = dir.path().join("state.json");
    let temporary = dir.path().join(".state.json.tmp");
    std::fs::write(&target, "old state").expect("write existing target");
    std::fs::write(&temporary, "new state").expect("write replacement");

    atomic_replace_file(&temporary, &target).expect("replace existing target");

    assert_eq!(
        std::fs::read_to_string(&target).expect("read replacement"),
        "new state"
    );
    assert!(
        !temporary.exists(),
        "successful replacement must consume the temporary file"
    );
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

    let _guard = ENV_LOCK.lock();
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
fn load_from_yaml_expands_cursor_style_env_secret_refs() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock();
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  openai:
    api_key: ${env:TEST_CURSOR_STYLE_OPENAI_KEY}
    base_url: https://${env:TEST_CURSOR_STYLE_OPENAI_HOST}/v1
"#,
    )
    .unwrap();

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("TEST_CURSOR_STYLE_OPENAI_KEY", "sk-cursor-style");
        std::env::set_var("TEST_CURSOR_STYLE_OPENAI_HOST", "api.cursor-style.test");
    }

    let runtime = load_from_yaml(&path).unwrap();
    let openai = runtime.llm_providers.get("openai").unwrap();
    assert_eq!(openai.api_key.as_deref(), Some("sk-cursor-style"));
    assert_eq!(
        openai.base_url.as_deref(),
        Some("https://api.cursor-style.test/v1")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("TEST_CURSOR_STYLE_OPENAI_KEY");
        std::env::remove_var("TEST_CURSOR_STYLE_OPENAI_HOST");
    }
}

#[test]
fn cursor_style_env_refs_trim_names_and_preserve_other_secret_sources() {
    let _guard = ENV_LOCK.lock();
    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::set_var("TEST_CURSOR_STYLE_TRIMMED", "trimmed-value") };

    assert_eq!(
        expand_env_refs_in_str("${env: TEST_CURSOR_STYLE_TRIMMED }"),
        "trimmed-value"
    );
    assert_eq!(
        expand_env_refs_in_str("${bitwarden:TEST_CURSOR_STYLE_TRIMMED}"),
        "${bitwarden:TEST_CURSOR_STYLE_TRIMMED}"
    );
    assert_eq!(
        expand_env_refs_in_str("${MY:WEIRD}"),
        "${MY:WEIRD}",
        "only lowercase source prefixes are secret-source references"
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("TEST_CURSOR_STYLE_TRIMMED") };
}

#[test]
fn load_from_yaml_keeps_unresolved_env_refs_verbatim() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock();
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

include!("tests/runtime_bridges.rs");
include!("tests/managed_overlays.rs");

include!("tests/user_config_patch.rs");

include!("tests/env_overrides.rs");

struct ExecutionModeEnvGuard {
    original: Option<std::ffi::OsString>,
}

impl ExecutionModeEnvGuard {
    fn capture() -> Self {
        Self {
            original: std::env::var_os(EXECUTION_MODE_ENV_KEY),
        }
    }
}

impl Drop for ExecutionModeEnvGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(value) => set_test_env(EXECUTION_MODE_ENV_KEY, value),
            None => remove_test_env(EXECUTION_MODE_ENV_KEY),
        }
    }
}

fn execution_mode_config(
    home: &std::path::Path,
    execution_mode: ExecutionModeConfigValue,
) -> GatewayConfig {
    let mut config = GatewayConfig::default();
    config.home_dir = Some(home.to_string_lossy().into_owned());
    config.execution_mode = execution_mode;
    config
}

fn write_active_execution_mode_profile(home: &std::path::Path, contents: &str) {
    let profiles = home.join("profiles");
    std::fs::create_dir_all(&profiles).expect("create profiles directory");
    std::fs::write(profiles.join(".active_profile"), "runtime\n")
        .expect("write active profile marker");
    std::fs::write(profiles.join("runtime.yaml"), contents).expect("write active profile");
}

#[test]
fn execution_mode_without_overrides_uses_manual_built_in_default() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    remove_test_env(EXECUTION_MODE_ENV_KEY);
    let directory = tempfile::tempdir().expect("create config directory");
    let config = execution_mode_config(directory.path(), ExecutionModeConfigValue::Absent);

    let resolved =
        resolve_runtime_execution_mode(&config, None).expect("resolve default execution mode");

    assert_eq!(resolved.mode, ExecutionMode::Manual);
    assert_eq!(resolved.source, ExecutionModeSource::BuiltInDefault);
}

#[test]
fn execution_mode_accepts_each_closed_value_from_cli() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    remove_test_env(EXECUTION_MODE_ENV_KEY);
    let directory = tempfile::tempdir().expect("create config directory");
    let config = execution_mode_config(directory.path(), ExecutionModeConfigValue::Absent);

    for (raw, expected) in [
        ("manual", ExecutionMode::Manual),
        ("reviewed", ExecutionMode::Reviewed),
        ("autonomous_local", ExecutionMode::AutonomousLocal),
        ("ci", ExecutionMode::Ci),
        ("remote_worker", ExecutionMode::RemoteWorker),
    ] {
        let resolved = resolve_runtime_execution_mode(&config, Some(raw))
            .expect("closed CLI mode should resolve");
        assert_eq!(resolved.mode, expected, "mode {raw}");
        assert_eq!(resolved.source, ExecutionModeSource::Cli, "mode {raw}");
    }
}

#[test]
fn execution_mode_accepts_each_closed_value_from_config_file() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    remove_test_env(EXECUTION_MODE_ENV_KEY);
    let directory = tempfile::tempdir().expect("create config directory");
    let path = directory.path().join("config.yaml");

    for (raw, expected) in [
        ("manual", ExecutionMode::Manual),
        ("reviewed", ExecutionMode::Reviewed),
        ("autonomous_local", ExecutionMode::AutonomousLocal),
        ("ci", ExecutionMode::Ci),
        ("remote_worker", ExecutionMode::RemoteWorker),
    ] {
        std::fs::write(&path, format!("execution_mode: {raw}\n"))
            .expect("write config-file execution mode");
        let mut config = load_from_yaml(&path).expect("load config-file execution mode");
        assert_eq!(config.execution_mode, ExecutionModeConfigValue::value(raw));
        config.home_dir = Some(directory.path().to_string_lossy().into_owned());
        let resolved = resolve_runtime_execution_mode(&config, None)
            .expect("closed config-file mode should resolve");
        assert_eq!(resolved.mode, expected, "mode {raw}");
        assert_eq!(
            resolved.source,
            ExecutionModeSource::ConfigFile,
            "mode {raw}"
        );
    }
}

#[test]
fn execution_mode_precedence_is_profile_config_environment_then_cli() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    let directory = tempfile::tempdir().expect("create config directory");
    write_active_execution_mode_profile(directory.path(), "execution_mode: manual\n");
    let config = execution_mode_config(directory.path(), "reviewed".into());
    set_test_env(EXECUTION_MODE_ENV_KEY, "autonomous_local");

    let resolved =
        resolve_runtime_execution_mode(&config, Some("ci")).expect("resolve all precedence layers");

    assert_eq!(resolved.mode, ExecutionMode::Ci);
    assert_eq!(resolved.source, ExecutionModeSource::Cli);
}

#[test]
fn execution_mode_absent_profile_falls_through_to_config_file() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    remove_test_env(EXECUTION_MODE_ENV_KEY);
    let directory = tempfile::tempdir().expect("create config directory");
    write_active_execution_mode_profile(directory.path(), "model: profile-model\n");
    let config = execution_mode_config(directory.path(), "reviewed".into());

    let resolved =
        resolve_runtime_execution_mode(&config, None).expect("resolve configured execution mode");

    assert_eq!(resolved.mode, ExecutionMode::Reviewed);
    assert_eq!(resolved.source, ExecutionModeSource::ConfigFile);
}

#[test]
fn execution_mode_rejects_empty_and_invalid_values_at_every_present_layer() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();

    for invalid in ["", "objective"] {
        for source in [
            ExecutionModeSource::Profile,
            ExecutionModeSource::ConfigFile,
            ExecutionModeSource::Environment,
            ExecutionModeSource::Cli,
        ] {
            let directory = tempfile::tempdir().expect("create config directory");
            let mut config =
                execution_mode_config(directory.path(), ExecutionModeConfigValue::Absent);
            let cli;

            match source {
                ExecutionModeSource::Profile => {
                    write_active_execution_mode_profile(
                        directory.path(),
                        &format!("execution_mode: {invalid:?}\n"),
                    );
                    config.execution_mode = "reviewed".into();
                    set_test_env(EXECUTION_MODE_ENV_KEY, "ci");
                    cli = Some("remote_worker");
                }
                ExecutionModeSource::ConfigFile => {
                    config.execution_mode = invalid.into();
                    set_test_env(EXECUTION_MODE_ENV_KEY, "ci");
                    cli = Some("remote_worker");
                }
                ExecutionModeSource::Environment => {
                    config.execution_mode = "reviewed".into();
                    set_test_env(EXECUTION_MODE_ENV_KEY, invalid);
                    cli = Some("remote_worker");
                }
                ExecutionModeSource::Cli => {
                    config.execution_mode = "reviewed".into();
                    set_test_env(EXECUTION_MODE_ENV_KEY, "ci");
                    cli = Some(invalid);
                }
                ExecutionModeSource::BuiltInDefault => unreachable!("not a configured layer"),
            }

            let error = resolve_runtime_execution_mode(&config, cli)
                .expect_err("present invalid execution mode must fail closed");
            assert!(
                error.to_string().contains(source.as_str()),
                "expected {source} failure for {invalid:?}, got {error}"
            );
        }
    }
}

#[test]
fn execution_mode_explicit_null_profile_fails_before_higher_valid_overrides() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    let directory = tempfile::tempdir().expect("create config directory");
    write_active_execution_mode_profile(directory.path(), "execution_mode:\n");
    let config = execution_mode_config(directory.path(), "reviewed".into());
    set_test_env(EXECUTION_MODE_ENV_KEY, "ci");

    let error = resolve_runtime_execution_mode(&config, Some("remote_worker"))
        .expect_err("explicit-null profile execution mode must fail closed");
    assert!(
        error
            .to_string()
            .contains(ExecutionModeSource::Profile.as_str()),
        "expected profile failure, got {error}"
    );
}

#[test]
fn execution_mode_invalid_config_file_forms_reject_on_load_with_source_and_path() {
    let directory = tempfile::tempdir().expect("create config directory");
    let path = directory.path().join("config.yaml");

    for (description, contents) in [
        ("explicit null", "execution_mode: null\n"),
        ("bare YAML null", "execution_mode:\n"),
        ("empty string", "execution_mode: \"\"\n"),
        ("unknown value", "execution_mode: objective\n"),
    ] {
        std::fs::write(&path, contents).expect("write config file");
        let error = load_from_yaml(&path)
            .expect_err("invalid config-file execution mode must reject during load");
        let rendered = error.to_string();
        assert!(
            rendered.contains(ExecutionModeSource::ConfigFile.as_str()),
            "expected config-file source for {description}, got {error}"
        );
        assert!(
            rendered.contains(path.to_string_lossy().as_ref()),
            "expected source path for {description}, got {error}"
        );
    }
}

#[test]
fn managed_overlay_rejects_explicit_null_execution_mode_without_mutating_base() {
    let mut config = GatewayConfig {
        execution_mode: ExecutionModeConfigValue::value("manual"),
        ..GatewayConfig::default()
    };

    let error =
        apply_managed_config_overlay(&mut config, &serde_json::json!({ "execution_mode": null }))
            .expect_err("managed explicit-null execution mode must fail closed");
    assert!(
        error
            .to_string()
            .contains(ExecutionModeSource::ConfigFile.as_str()),
        "expected config-file source, got {error}"
    );
    assert_eq!(
        config.execution_mode,
        ExecutionModeConfigValue::value("manual")
    );
}

#[test]
fn execution_mode_file_load_matrix_handles_missing_and_all_invalid_forms() {
    let directory = tempfile::tempdir().expect("create config directory");
    let path = directory.path().join("config.yaml");

    for (description, contents, expected) in [
        ("missing", "{}\n", Some(ExecutionModeConfigValue::Absent)),
        (
            "closed value",
            "execution_mode: manual\n",
            Some(ExecutionModeConfigValue::value("manual")),
        ),
        ("explicit null", "execution_mode: null\n", None),
        ("empty value", "execution_mode: \"\"\n", None),
        ("unknown value", "execution_mode: objective\n", None),
    ] {
        std::fs::write(&path, contents).expect("write config file");
        match expected {
            Some(expected) => {
                let loaded =
                    load_from_yaml(&path).expect("missing or closed execution mode should load");
                assert_eq!(loaded.execution_mode, expected, "{description}");
            }
            None => {
                let error = load_from_yaml(&path)
                    .expect_err("present invalid execution mode must reject on load");
                assert!(
                    error
                        .to_string()
                        .contains(ExecutionModeSource::ConfigFile.as_str()),
                    "expected config-file source for {description}, got {error}"
                );
            }
        }
    }
}

#[test]
fn validate_config_rejects_all_invalid_typed_execution_mode_states() {
    for value in [
        ExecutionModeConfigValue::ExplicitNull,
        ExecutionModeConfigValue::value(""),
        ExecutionModeConfigValue::value("objective"),
    ] {
        let config = GatewayConfig {
            execution_mode: value,
            ..GatewayConfig::default()
        };
        let error = validate_config(&config)
            .expect_err("typed present invalid execution mode must fail validation");
        assert!(
            error
                .to_string()
                .contains(ExecutionModeSource::ConfigFile.as_str()),
            "expected config-file source, got {error}"
        );
    }
}

#[test]
fn execution_mode_source_winner_matrix_covers_every_precedence_layer() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();

    remove_test_env(EXECUTION_MODE_ENV_KEY);
    let built_in_directory = tempfile::tempdir().expect("create built-in directory");
    let built_in = resolve_runtime_execution_mode(
        &execution_mode_config(built_in_directory.path(), ExecutionModeConfigValue::Absent),
        None,
    )
    .expect("built-in default should resolve");
    assert_eq!(
        built_in,
        hermes_core::ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        }
    );

    let profile_directory = tempfile::tempdir().expect("create profile directory");
    write_active_execution_mode_profile(profile_directory.path(), "execution_mode: reviewed\n");
    let profile = resolve_runtime_execution_mode(
        &execution_mode_config(profile_directory.path(), ExecutionModeConfigValue::Absent),
        None,
    )
    .expect("profile mode should resolve");
    assert_eq!(
        profile,
        hermes_core::ResolvedExecutionMode {
            mode: ExecutionMode::Reviewed,
            source: ExecutionModeSource::Profile,
        }
    );

    let config_directory = tempfile::tempdir().expect("create config directory");
    let config = resolve_runtime_execution_mode(
        &execution_mode_config(
            config_directory.path(),
            ExecutionModeConfigValue::value("autonomous_local"),
        ),
        None,
    )
    .expect("config-file mode should resolve");
    assert_eq!(
        config,
        hermes_core::ResolvedExecutionMode {
            mode: ExecutionMode::AutonomousLocal,
            source: ExecutionModeSource::ConfigFile,
        }
    );

    let environment_directory = tempfile::tempdir().expect("create environment directory");
    set_test_env(EXECUTION_MODE_ENV_KEY, "ci");
    let environment = resolve_runtime_execution_mode(
        &execution_mode_config(
            environment_directory.path(),
            ExecutionModeConfigValue::value("reviewed"),
        ),
        None,
    )
    .expect("environment mode should resolve");
    assert_eq!(
        environment,
        hermes_core::ResolvedExecutionMode {
            mode: ExecutionMode::Ci,
            source: ExecutionModeSource::Environment,
        }
    );

    let cli_directory = tempfile::tempdir().expect("create cli directory");
    let cli = resolve_runtime_execution_mode(
        &execution_mode_config(
            cli_directory.path(),
            ExecutionModeConfigValue::value("reviewed"),
        ),
        Some("remote_worker"),
    )
    .expect("CLI mode should resolve");
    assert_eq!(
        cli,
        hermes_core::ResolvedExecutionMode {
            mode: ExecutionMode::RemoteWorker,
            source: ExecutionModeSource::Cli,
        }
    );
}

#[test]
fn execution_mode_diagnostic_reports_all_layers_and_lower_error_provenance() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    let directory = tempfile::tempdir().expect("create config directory");
    write_active_execution_mode_profile(directory.path(), "execution_mode: manual\n");
    set_test_env(EXECUTION_MODE_ENV_KEY, "autonomous_local");
    let config = execution_mode_config(
        directory.path(),
        ExecutionModeConfigValue::value("reviewed"),
    );

    let diagnostic = diagnose_runtime_execution_mode(&config, Some("ci"));
    assert_eq!(diagnostic.mode, Some(ExecutionMode::Ci));
    assert_eq!(diagnostic.source, Some(ExecutionModeSource::Cli));
    assert!(diagnostic.error.is_none());
    let expected = [
        (ExecutionModeSource::BuiltInDefault, "manual"),
        (ExecutionModeSource::Profile, "manual"),
        (ExecutionModeSource::ConfigFile, "reviewed"),
        (ExecutionModeSource::Environment, "autonomous_local"),
        (ExecutionModeSource::Cli, "ci"),
    ];
    for (layer, (source, value)) in diagnostic.layers.iter().zip(expected) {
        assert_eq!(layer.source, source);
        assert_eq!(
            layer.presence,
            ExecutionModeLayerPresence::Value(value.to_string())
        );
        assert!(layer.error.is_none(), "unexpected layer error: {layer:?}");
    }

    write_active_execution_mode_profile(directory.path(), "execution_mode: objective\n");
    let invalid = diagnose_runtime_execution_mode(&config, Some("ci"));
    assert_eq!(invalid.mode, None);
    assert_eq!(invalid.source, None);
    assert_eq!(
        invalid.error.as_ref().map(|error| error.source),
        Some(ExecutionModeSource::Profile)
    );
    assert!(
        invalid.layers[1].path.is_some(),
        "profile diagnostic must retain its source path"
    );
    assert_eq!(
        invalid.layers[2].presence,
        ExecutionModeLayerPresence::Value("reviewed".to_string())
    );
    assert_eq!(
        invalid.layers[3].presence,
        ExecutionModeLayerPresence::Value("autonomous_local".to_string())
    );
    assert_eq!(
        invalid.layers[4].presence,
        ExecutionModeLayerPresence::Value("ci".to_string())
    );
}

#[test]
fn user_config_unknown_keys_are_strict_by_default_with_explicit_compatibility_escape_hatch() {
    let directory = tempfile::tempdir().expect("create config directory");
    let path = directory.path().join("config.yaml");
    std::fs::write(&path, "unknown_user_key: true\n").expect("write config file");

    let error = load_user_config_file(&path)
        .expect_err("strict user config loading must reject unknown keys");
    let rendered = error.to_string();
    assert!(
        rendered.contains("unknown config key 'unknown_user_key'"),
        "expected strict unknown-key error, got {error}"
    );
    assert!(
        rendered.contains(path.to_string_lossy().as_ref()),
        "expected source path, got {error}"
    );

    let compatibility =
        load_user_config_file_with_options(&path, UserConfigLoadOptions::compatibility())
            .expect("compatibility option must be an explicit user-only escape hatch");
    assert_eq!(
        compatibility.execution_mode,
        ExecutionModeConfigValue::Absent
    );

    std::fs::write(&path, "unknown_null_key: null\n").expect("write null unknown key");
    let error = load_from_yaml(&path)
        .expect_err("normalization must not silently drop an unknown null key");
    assert!(
        error
            .to_string()
            .contains("unknown config key 'unknown_null_key'"),
        "expected strict unknown-null failure, got {error}"
    );
}

#[test]
fn cli_config_example_is_strictly_parseable_and_declares_execution_mode() {
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("cli-config.yaml.example");
    let config = load_from_yaml(&example).expect("example config must remain strictly parseable");

    assert_eq!(
        config.execution_mode,
        ExecutionModeConfigValue::value("manual")
    );
}

#[test]
fn json_config_loader_rejects_unknown_and_invalid_execution_mode_with_path() {
    let directory = tempfile::tempdir().expect("create config directory");
    let path = directory.path().join("gateway.json");

    std::fs::write(&path, r#"{"unknown_json_key":true}"#).expect("write unknown JSON config");
    let unknown = load_from_json(&path).expect_err("strict JSON loader must reject unknown keys");
    assert!(
        unknown
            .to_string()
            .contains("unknown config key 'unknown_json_key'"),
        "expected strict unknown JSON error, got {unknown}"
    );

    std::fs::write(&path, r#"{"execution_mode":"objective"}"#)
        .expect("write invalid execution-mode JSON config");
    let invalid =
        load_from_json(&path).expect_err("invalid JSON execution mode must reject during load");
    let rendered = invalid.to_string();
    assert!(
        rendered.contains(ExecutionModeSource::ConfigFile.as_str()),
        "expected config-file source, got {invalid}"
    );
    assert!(
        rendered.contains(path.to_string_lossy().as_ref()),
        "expected JSON source path, got {invalid}"
    );
}

#[test]
fn load_config_rejects_lower_invalid_file_even_with_valid_higher_environment() {
    let _env_lock = ENV_LOCK.lock();
    let _env_guard = ExecutionModeEnvGuard::capture();
    let directory = tempfile::tempdir().expect("create config directory");
    let path = directory.path().join("config.yaml");
    std::fs::write(&path, "execution_mode: objective\n").expect("write invalid config file");
    set_test_env(EXECUTION_MODE_ENV_KEY, "ci");

    let error = load_config(Some(directory.path().to_string_lossy().as_ref()))
        .expect_err("a valid environment override must not hide invalid file config");
    let rendered = error.to_string();
    assert!(
        rendered.contains(ExecutionModeSource::ConfigFile.as_str()),
        "expected config-file source, got {error}"
    );
    assert!(
        rendered.contains(path.to_string_lossy().as_ref()),
        "expected lower-layer source path, got {error}"
    );
}

#[test]
fn load_config_retains_only_a_previously_valid_user_config_after_transient_rejection() {
    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _env_guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();

    let home = tempfile::tempdir().expect("create config home");
    let path = home.path().join("config.yaml");
    std::fs::write(
        &path,
        "max_turns: 17\napproval:\n  enabled: true\n  require_approval: true\n",
    )
    .expect("write valid config");

    let initial = load_config(Some(home.path().to_string_lossy().as_ref()))
        .expect("initial valid configuration loads");
    assert_eq!(initial.max_turns, 17);
    assert!(initial.approval.require_approval);

    std::fs::write(&path, "max_turns: [\n").expect("write malformed replacement");
    let retained = load_config(Some(home.path().to_string_lossy().as_ref()))
        .expect("previously validated config must survive a transient malformed replacement");
    assert_eq!(retained.max_turns, 17);
    assert!(retained.approval.require_approval);

    std::fs::write(&path, "max_turns: 23\n").expect("repair config");
    let repaired = load_config(Some(home.path().to_string_lossy().as_ref()))
        .expect("repaired config loads and replaces the cached snapshot");
    assert_eq!(repaired.max_turns, 23);

    let fresh_home = tempfile::tempdir().expect("create fresh config home");
    std::fs::write(fresh_home.path().join("config.yaml"), "max_turns: [\n")
        .expect("write first-load malformed config");
    assert!(
        load_config(Some(fresh_home.path().to_string_lossy().as_ref())).is_err(),
        "a malformed first load must remain fail-closed"
    );

    clear_managed_scope_test_env();
}

#[test]
fn raw_effective_config_retains_only_a_previously_valid_mapping_after_transient_rejection() {
    let _global_guard = crate::managed_gateway::test_lock::lock();
    let _env_guard = ENV_LOCK.lock();
    clear_managed_scope_test_env();

    let home = tempfile::tempdir().expect("create config home");
    let path = home.path().join("config.yaml");
    std::fs::write(&path, "upstream_opaque:\n  nested: retained\n")
        .expect("write valid opaque config");

    let initial = load_effective_config_yaml_value(&path).expect("read valid raw config");
    assert_eq!(
        initial
            .pointer("/upstream_opaque/nested")
            .and_then(serde_json::Value::as_str),
        Some("retained")
    );

    std::fs::write(&path, "upstream_opaque: [\n").expect("write malformed raw replacement");
    let retained = load_effective_config_yaml_value(&path)
        .expect("previously valid raw config must survive a transient malformed replacement");
    assert_eq!(
        retained
            .pointer("/upstream_opaque/nested")
            .and_then(serde_json::Value::as_str),
        Some("retained")
    );

    let fresh_home = tempfile::tempdir().expect("create fresh config home");
    let fresh_path = fresh_home.path().join("config.yaml");
    std::fs::write(&fresh_path, "upstream_opaque: [\n")
        .expect("write first-load malformed raw config");
    assert!(
        load_effective_config_yaml_value(&fresh_path).is_none(),
        "a malformed first raw load must remain fail-closed"
    );

    clear_managed_scope_test_env();
}
