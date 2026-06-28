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
