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
