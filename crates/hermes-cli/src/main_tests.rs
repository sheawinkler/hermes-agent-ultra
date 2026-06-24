use aes_gcm::Aes256Gcm;
use aes_gcm::aead::Aead;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use clap::Parser;
use hermes_auth::{AuthManager, FileTokenStore, OAuthCredential};
use hermes_cli::auth::{
    ANTHROPIC_OAUTH_CLIENT_ID, ANTHROPIC_OAUTH_TOKEN_URL, CODEX_OAUTH_CLIENT_ID,
    CODEX_OAUTH_TOKEN_URL,
};
use hermes_cli::cli::Cli;
use hermes_config::session::SessionConfig;
use hermes_config::{GatewayConfig, PlatformConfig, load_user_config_file};
use hermes_core::AgentError;
use hermes_gateway::dm::DmManager;
use hermes_gateway::{
    Gateway, GatewayRuntimeContext, SessionManager, gateway::GatewayConfig as RuntimeGatewayConfig,
};
use std::sync::{Arc, Mutex, OnceLock};

use crate::auth_main::{
    auth_verify_source, gateway_platform_provider_key, hydrate_provider_env_from_vault_for_cli,
    mask_secret, normalize_auth_provider, oauth_refresh_config_for_provider, provider_env_var,
    qqbot_connect_url, qqbot_decrypt_secret, qqbot_extract_i64, resolve_auth_type_for_provider,
    secret_provider_aliases, wecom_qr_page_url,
};
use crate::cli_setup::run_model;
use crate::doctor::{
    DebugLogSnapshot, PendingPasteDelete, ReplayIntegritySummary,
    best_effort_sweep_expired_pending_pastes, build_elite_doctor_diagnostics,
    capture_debug_log_snapshot, debug_pending_pastes_path, replay_integrity_for_file,
    replay_manifest_json, run_doctor_self_heal, sweep_expired_pending_pastes,
};
use crate::profile_main::{run_profile, validate_profile_name, write_active_profile_name};
use crate::{
    gateway_platform_menu_label, hermes_state_root, infer_oauth_provider_from_error_message,
    oneshot_auth_is_refreshable, oneshot_auto_verify_oauth_provider, query_is_local_slash_command,
};
use hermes_cli::gateway_main::{
    GATEWAY_PLATFORM_CATALOG, apply_telegram_allowlists, build_api_server_config,
    gateway_agent_signature, gateway_requirement_issues, matrix_home_room_for_platform,
    register_gateway_adapters, run_sessions_db_auto_maintenance,
};
use hermes_cli::paths::CliStateRoot;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned")
}

#[test]
fn gateway_platform_menu_label_marks_configured_platforms() {
    let entry = &GATEWAY_PLATFORM_CATALOG[0];
    assert_eq!(entry.key, "telegram");
    let mut configured = make_platform(true, Some("tg-token"));
    configured.allowed_users = vec!["123456789".to_string()];
    let label = gateway_platform_menu_label(entry, Some(&configured));
    assert!(label.contains("Telegram"));
    assert!(label.contains("(configured)"));

    configured.allowed_users.clear();
    let label = gateway_platform_menu_label(entry, Some(&configured));
    assert!(label.contains("(not configured)"));

    configured.token = None;
    let label = gateway_platform_menu_label(entry, Some(&configured));
    assert!(label.contains("(not configured)"));
}

#[test]
fn apply_telegram_allowlists_sets_policy_fields() {
    let mut platform = PlatformConfig::default();
    apply_telegram_allowlists(&mut platform, &["111".into(), "222".into()]);
    assert_eq!(platform.allowed_users, vec!["111", "222"]);
    assert_eq!(
        platform.extra.get("dm_policy").and_then(|v| v.as_str()),
        Some("allowlist")
    );
    let allow_from = platform
        .extra
        .get("allow_from")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();
    assert_eq!(allow_from, vec!["111", "222"]);
}

fn cli_for_temp_state_root(temp_root: &std::path::Path) -> Cli {
    use clap::Parser;
    Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        temp_root.to_str().expect("utf8 path"),
    ])
}

fn make_platform(enabled: bool, token: Option<&str>) -> PlatformConfig {
    let mut cfg = PlatformConfig {
        enabled,
        ..Default::default()
    };
    if let Some(t) = token {
        cfg.token = Some(t.to_string());
    }
    cfg
}

fn make_gateway() -> Arc<Gateway> {
    Arc::new(Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        DmManager::with_pair_behavior(),
        hermes_gateway::gateway::GatewayConfig::default(),
    ))
}

#[test]
fn gateway_agent_signature_changes_when_user_changes() {
    let cfg = GatewayConfig::default();
    let mut ctx_a = GatewayRuntimeContext::default();
    ctx_a.session_key = "wecom:room-1".to_string();
    ctx_a.platform = "wecom".to_string();
    ctx_a.user_id = "alice".to_string();
    let mut ctx_b = ctx_a.clone();
    ctx_b.user_id = "bob".to_string();
    assert_ne!(
        gateway_agent_signature(&cfg, &ctx_a),
        gateway_agent_signature(&cfg, &ctx_b)
    );
}

#[test]
fn gateway_agent_signature_changes_when_personality_changes() {
    let cfg = GatewayConfig::default();
    let mut ctx_a = GatewayRuntimeContext::default();
    ctx_a.session_key = "wecom:room-1".to_string();
    ctx_a.platform = "wecom".to_string();
    ctx_a.user_id = "alice".to_string();
    ctx_a.personality = Some("default".to_string());
    let mut ctx_b = ctx_a.clone();
    ctx_b.personality = Some("strict".to_string());
    assert_ne!(
        gateway_agent_signature(&cfg, &ctx_a),
        gateway_agent_signature(&cfg, &ctx_b)
    );
}

#[tokio::test]
async fn run_model_persists_default_model_to_config_yaml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    run_model(
        cli.clone(),
        Some("nous:nousresearch/hermes-4-70b".to_string()),
    )
    .await
    .expect("run model");

    let cfg = load_user_config_file(&tmp.path().join("config.yaml")).expect("load config");
    assert_eq!(cfg.model.as_deref(), Some("nous:nousresearch/hermes-4-70b"));
}

#[test]
fn mask_secret_hides_token_body() {
    let raw = "abcdefgh1234567890";
    let masked = mask_secret(raw);
    assert!(!masked.contains(raw));
    assert!(masked.starts_with("abcd"));
    assert!(masked.ends_with("7890"));
    assert!(masked.contains("***"));
}

#[test]
fn api_server_config_defaults_to_loopback() {
    let platform = PlatformConfig {
        enabled: true,
        ..Default::default()
    };
    let cfg = build_api_server_config(&platform);
    assert_eq!(cfg.host, "127.0.0.1");
    assert_eq!(cfg.port, 8090);
    assert_eq!(cfg.auth_token, None);
}

#[test]
fn api_server_config_honors_overrides_and_token_precedence() {
    let mut platform = PlatformConfig {
        enabled: true,
        token: Some("platform-token".to_string()),
        ..Default::default()
    };
    platform
        .extra
        .insert("host".to_string(), serde_json::json!("0.0.0.0"));
    platform
        .extra
        .insert("port".to_string(), serde_json::json!(9123));
    platform
        .extra
        .insert("auth_token".to_string(), serde_json::json!("extra-token"));

    let cfg = build_api_server_config(&platform);
    assert_eq!(cfg.host, "0.0.0.0");
    assert_eq!(cfg.port, 9123);
    assert_eq!(cfg.auth_token.as_deref(), Some("platform-token"));
}

#[test]
fn auth_provider_aliases_cover_primary_chains() {
    assert_eq!(normalize_auth_provider("tg"), "telegram");
    assert_eq!(normalize_auth_provider("wechat"), "weixin");
    assert_eq!(normalize_auth_provider("wx"), "weixin");
    assert_eq!(normalize_auth_provider("claude"), "anthropic");
    assert_eq!(normalize_auth_provider("codex"), "openai-codex");
    assert_eq!(normalize_auth_provider("openai-oauth"), "openai");
    assert_eq!(normalize_auth_provider("qwen-cli"), "qwen-oauth");
    assert_eq!(normalize_auth_provider("gemini-cli"), "google-gemini-cli");
    assert_eq!(normalize_auth_provider("step-plan"), "stepfun");
    assert_eq!(normalize_auth_provider("aigateway"), "ai-gateway");
    assert_eq!(normalize_auth_provider("moonshot"), "kimi-coding");
    assert_eq!(normalize_auth_provider("z-ai"), "zai");
    assert_eq!(normalize_auth_provider("grok"), "xai");
    assert_eq!(normalize_auth_provider("hf"), "huggingface");
    assert_eq!(normalize_auth_provider("ollama"), "ollama-local");
    assert_eq!(normalize_auth_provider("llama.cpp"), "llama-cpp");
    assert_eq!(normalize_auth_provider("ollvm"), "vllm");
    assert_eq!(normalize_auth_provider("llvm"), "vllm");
    assert_eq!(normalize_auth_provider("mlx-lm"), "mlx");
    assert_eq!(normalize_auth_provider("ane"), "apple-ane");
    assert_eq!(normalize_auth_provider("text-generation-inference"), "tgi");
    assert_eq!(normalize_auth_provider("api-server"), "api_server");
    assert_eq!(normalize_auth_provider("mm"), "mattermost");
}

#[test]
fn oneshot_auto_verify_provider_detects_nous_401_errors() {
    let err = AgentError::LlmApi(
        "API error 401 Unauthorized: https://portal.nousresearch.com".to_string(),
    );
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&err, Some("nous"), Some("nous:openai/gpt-5.5")),
        Some("nous".to_string())
    );
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&err, None, Some("nous:moonshotai/kimi-k2.6")),
        Some("nous".to_string())
    );
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&err, None, None),
        Some("nous".to_string())
    );
}

#[test]
fn oneshot_auto_verify_provider_supports_core_oauth_providers() {
    let openai = AgentError::LlmApi("API error 401 Unauthorized: auth.openai.com".to_string());
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&openai, Some("openai"), Some("openai:gpt-5.5")),
        Some("openai".to_string())
    );
    let codex = AgentError::LlmApi("API error 401 Unauthorized: chatgpt.com codex".to_string());
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&codex, None, Some("openai-codex:codex-mini")),
        Some("openai-codex".to_string())
    );
    let anthropic = AgentError::LlmApi(
        "API error 401 Unauthorized: console.anthropic.com token expired".to_string(),
    );
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&anthropic, Some("claude"), None),
        Some("anthropic".to_string())
    );
    let gemini = AgentError::LlmApi(
        "API error 401 Unauthorized: oauth2.googleapis.com invalid_grant".to_string(),
    );
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&gemini, Some("gemini-cli"), None),
        Some("google-gemini-cli".to_string())
    );
    let qwen =
        AgentError::LlmApi("API error 401 Unauthorized: chat.qwen.ai token expired".to_string());
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&qwen, Some("qwen-cli"), None),
        Some("qwen-oauth".to_string())
    );
}

#[test]
fn oneshot_auto_verify_provider_ignores_non_oauth_or_non_auth_errors() {
    let not_auth = AgentError::LlmApi("API error 404 Not Found".to_string());
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&not_auth, Some("nous"), Some("nous:openai/gpt-5.5")),
        None
    );

    let other_provider = AgentError::LlmApi(
        "API error 401 Unauthorized: provider openrouter token expired".to_string(),
    );
    assert_eq!(
        oneshot_auto_verify_oauth_provider(
            &other_provider,
            Some("openrouter"),
            Some("openrouter:openai/gpt-4o")
        ),
        None
    );

    let missing_signal = AgentError::LlmApi("API error 500 Internal Server Error".to_string());
    assert_eq!(
        oneshot_auto_verify_oauth_provider(&missing_signal, Some("openai"), Some("openai:gpt-5.5")),
        None
    );
}

#[test]
fn oneshot_auth_is_refreshable_detects_auth_signals() {
    assert!(oneshot_auth_is_refreshable(
        "api error 401 unauthorized token expired"
    ));
    assert!(oneshot_auth_is_refreshable("invalid_grant"));
    assert!(!oneshot_auth_is_refreshable("api error 404 not found"));
}

#[test]
fn infer_oauth_provider_from_error_message_maps_known_hosts() {
    assert_eq!(
        infer_oauth_provider_from_error_message("portal.nousresearch.com unauthorized"),
        Some("nous".to_string())
    );
    assert_eq!(
        infer_oauth_provider_from_error_message("auth.openai.com unauthorized"),
        Some("openai".to_string())
    );
    assert_eq!(
        infer_oauth_provider_from_error_message("chatgpt.com codex token expired"),
        Some("openai-codex".to_string())
    );
    assert_eq!(
        infer_oauth_provider_from_error_message("console.anthropic.com invalid token"),
        Some("anthropic".to_string())
    );
    assert_eq!(
        infer_oauth_provider_from_error_message("oauth2.googleapis.com invalid_grant"),
        Some("google-gemini-cli".to_string())
    );
    assert_eq!(
        infer_oauth_provider_from_error_message("chat.qwen.ai invalid token"),
        Some("qwen-oauth".to_string())
    );
    assert_eq!(
        infer_oauth_provider_from_error_message("openrouter.ai unauthorized"),
        None
    );
}

#[test]
fn resolve_auth_type_prefers_oauth_for_supported_providers() {
    assert_eq!(resolve_auth_type_for_provider("nous", None), "oauth");
    assert_eq!(
        resolve_auth_type_for_provider("openai-codex", None),
        "oauth"
    );
    assert_eq!(resolve_auth_type_for_provider("qwen-oauth", None), "oauth");
    assert_eq!(
        resolve_auth_type_for_provider("google-gemini-cli", None),
        "oauth"
    );
    assert_eq!(resolve_auth_type_for_provider("anthropic", None), "oauth");
    assert_eq!(resolve_auth_type_for_provider("openai", None), "oauth");
    assert_eq!(
        resolve_auth_type_for_provider("openai", Some("API-KEY")),
        "api_key"
    );
    assert_eq!(
        resolve_auth_type_for_provider("openai", Some("oauth")),
        "oauth"
    );
}

#[test]
fn oauth_refresh_config_defaults_cover_core_oauth_providers() {
    let _guard = env_lock();
    hermes_cli::env_vars::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL");
    hermes_cli::env_vars::remove_var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL");
    hermes_cli::env_vars::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID");
    hermes_cli::env_vars::remove_var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID");
    hermes_cli::env_vars::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
    hermes_cli::env_vars::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");

    let (openai_token_url, openai_client_id) =
        oauth_refresh_config_for_provider("openai").expect("openai config");
    assert_eq!(openai_token_url, CODEX_OAUTH_TOKEN_URL);
    assert_eq!(openai_client_id, CODEX_OAUTH_CLIENT_ID);

    let (codex_token_url, codex_client_id) =
        oauth_refresh_config_for_provider("openai-codex").expect("codex config");
    assert_eq!(codex_token_url, CODEX_OAUTH_TOKEN_URL);
    assert_eq!(codex_client_id, CODEX_OAUTH_CLIENT_ID);

    let (anthropic_token_url, anthropic_client_id) =
        oauth_refresh_config_for_provider("anthropic").expect("anthropic config");
    assert_eq!(anthropic_token_url, ANTHROPIC_OAUTH_TOKEN_URL);
    assert_eq!(anthropic_client_id, ANTHROPIC_OAUTH_CLIENT_ID);

    assert!(oauth_refresh_config_for_provider("nous").is_none());
}

#[test]
fn auth_verify_source_priority_is_env_then_store_then_state() {
    assert_eq!(auth_verify_source(true, true, true), "env");
    assert_eq!(auth_verify_source(false, true, true), "token_store");
    assert_eq!(auth_verify_source(false, false, true), "auth_json");
    assert_eq!(auth_verify_source(false, false, false), "none");
}

#[test]
fn provider_env_var_maps_stepfun() {
    assert_eq!(provider_env_var("stepfun"), Some("STEPFUN_API_KEY"));
    assert_eq!(provider_env_var("step"), None);
    assert_eq!(
        provider_env_var("openai-codex"),
        Some("HERMES_OPENAI_CODEX_API_KEY")
    );
    assert_eq!(
        provider_env_var("qwen-oauth"),
        Some("HERMES_QWEN_OAUTH_API_KEY")
    );
    assert_eq!(
        provider_env_var("google-gemini-cli"),
        Some("HERMES_GEMINI_OAUTH_API_KEY")
    );
    assert_eq!(secret_provider_aliases("stepfun"), vec!["stepfun", "step"]);
    assert_eq!(
        secret_provider_aliases("claude"),
        vec!["anthropic", "claude", "claude-code"]
    );
    assert_eq!(provider_env_var("ollama"), Some("OLLAMA_LOCAL_API_KEY"));
    assert_eq!(provider_env_var("llama.cpp"), Some("LLAMA_CPP_API_KEY"));
    assert_eq!(provider_env_var("ollvm"), Some("VLLM_API_KEY"));
    assert_eq!(provider_env_var("mlx-lm"), Some("MLX_API_KEY"));
    assert_eq!(provider_env_var("ane"), Some("APPLE_ANE_API_KEY"));
    assert_eq!(
        provider_env_var("text-generation-inference"),
        Some("TGI_API_KEY")
    );
}

#[test]
fn matrix_home_room_prefers_platform_config_then_env_fallback() {
    let _guard = env_lock();
    let previous = std::env::var("MATRIX_HOME_ROOM").ok();

    let mut platform = PlatformConfig::default();
    platform
        .extra
        .insert("room_id".to_string(), serde_json::json!("!cfg:matrix.org"));
    hermes_cli::env_vars::set_var("MATRIX_HOME_ROOM", "!env:matrix.org");
    assert_eq!(
        matrix_home_room_for_platform(&platform).as_deref(),
        Some("!cfg:matrix.org")
    );

    platform.extra.remove("room_id");
    assert_eq!(
        matrix_home_room_for_platform(&platform).as_deref(),
        Some("!env:matrix.org")
    );

    match previous {
        Some(value) => hermes_cli::env_vars::set_var("MATRIX_HOME_ROOM", value),
        None => hermes_cli::env_vars::remove_var("MATRIX_HOME_ROOM"),
    }
}

#[test]
fn setup_model_choice_supports_nous() {
    let option = &crate::setup::SETUP_MODEL_OPTIONS
        [crate::setup::default_setup_model_choice().saturating_sub(1)];
    assert_eq!(option.model, "nous:openai/gpt-5.5-pro");
    assert_eq!(option.provider, "nous");
}

#[test]
fn setup_provider_defaults_are_unique_and_include_nous() {
    let providers = crate::setup::setup_provider_defaults();
    assert!(!providers.is_empty());
    let mut seen = std::collections::BTreeSet::new();
    for option in providers {
        assert!(
            seen.insert(option.provider),
            "duplicate provider {}",
            option.provider
        );
    }
    assert!(seen.contains("nous"));
}

#[test]
fn setup_default_model_pick_index_matches_provider_prefixed_target() {
    let suggested = vec![
        "nousresearch/hermes-3-llama-3.1-405b".to_string(),
        "openai/gpt-5.5-pro".to_string(),
        "moonshotai/kimi-k2.6".to_string(),
    ];
    let idx =
        crate::setup::setup_default_model_pick_index("nous", "nous:openai/gpt-5.5-pro", &suggested);
    assert_eq!(idx, 1);
}

#[test]
fn setup_default_model_pick_index_uses_nous_kimi_fallback_when_target_missing() {
    let suggested = vec![
        "nousresearch/hermes-3-llama-3.1-405b".to_string(),
        "moonshotai/kimi-k2.6".to_string(),
        "openai/gpt-5.5".to_string(),
    ];
    let idx =
        crate::setup::setup_default_model_pick_index("nous", "nous:nonexistent/model", &suggested);
    assert_eq!(idx, 1);
}

#[test]
fn setup_default_model_pick_index_falls_back_to_zero_for_non_nous() {
    let suggested = vec![
        "gpt-4o".to_string(),
        "gpt-4o-mini".to_string(),
        "gpt-5.4".to_string(),
    ];
    let idx = crate::setup::setup_default_model_pick_index("openai", "openai:not-real", &suggested);
    assert_eq!(idx, 0);
}

#[test]
fn setup_provider_env_keys_include_nous() {
    assert_eq!(crate::setup::setup_provider_display("nous"), "Nous");
    assert_eq!(
        crate::setup::setup_provider_env_keys("nous"),
        &["NOUS_API_KEY"]
    );
    assert_eq!(
        crate::setup::setup_provider_env_keys("ollama-local"),
        &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"]
    );
    assert_eq!(
        crate::setup::setup_provider_default_base_url("vllm"),
        Some("http://127.0.0.1:8000/v1")
    );
    assert!(!crate::setup::setup_provider_requires_api_key(
        "ollama-local"
    ));
    assert!(!crate::setup::setup_provider_requires_api_key("apple-ane"));
    assert!(crate::setup::setup_provider_requires_api_key("openai"));
    assert_eq!(
        crate::setup::setup_provider_display("alibaba"),
        "Alibaba Cloud DashScope"
    );
    assert_eq!(
        crate::setup::setup_provider_env_keys("google-gemini-cli"),
        &["HERMES_GEMINI_OAUTH_API_KEY"]
    );
    assert_eq!(
        crate::setup::setup_provider_default_base_url("ai-gateway"),
        Some("https://ai-gateway.vercel.sh/v1")
    );
    assert!(
        crate::setup::SETUP_MODEL_OPTIONS.len() >= 20,
        "setup provider catalog unexpectedly narrow"
    );
}

#[test]
fn oauth_provider_set_matches_snapshot_registry() {
    let actual: std::collections::BTreeSet<&str> = hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
        .iter()
        .copied()
        .collect();
    let expected_minimum: std::collections::BTreeSet<&str> = [
        "anthropic",
        "nous",
        "openai-codex",
        "qwen-oauth",
        "google-gemini-cli",
    ]
    .into_iter()
    .collect();
    let missing: Vec<&str> = expected_minimum
        .iter()
        .copied()
        .filter(|provider| !actual.contains(provider))
        .collect();
    assert!(
        missing.is_empty(),
        "missing upstream oauth providers: {:?}",
        missing
    );
    assert!(
        actual.contains("openai"),
        "OpenAI OAuth should be enabled in Hermes Ultra"
    );
}

#[tokio::test]
async fn hydrate_provider_env_from_vault_overrides_oauth_provider_env() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let vault_path = CliStateRoot::from_state_root(&hermes_state_root(&cli)).secret_vault();
    let store = FileTokenStore::new(vault_path).await.expect("vault store");
    let manager = AuthManager::new(store);
    manager
        .save_credential(OAuthCredential {
            provider: "nous".to_string(),
            access_token: "vault-good-key".to_string(),
            refresh_token: None,
            token_type: "bearer".to_string(),
            scope: None,
            expires_at: None,
        })
        .await
        .expect("save vault credential");

    let previous = std::env::var("NOUS_API_KEY").ok();
    hermes_cli::env_vars::set_var("NOUS_API_KEY", "env-stale-key");

    hydrate_provider_env_from_vault_for_cli(&cli)
        .await
        .expect("hydrate env");
    assert_eq!(
        std::env::var("NOUS_API_KEY").as_deref(),
        Ok("vault-good-key")
    );

    match previous {
        Some(value) => hermes_cli::env_vars::set_var("NOUS_API_KEY", value),
        None => hermes_cli::env_vars::remove_var("NOUS_API_KEY"),
    }
}

#[test]
fn read_env_key_treats_empty_values_as_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_file = tmp.path().join(".env");
    std::fs::write(
        &env_file,
        "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='   '\nOPENAI_API_KEY=real-key\n",
    )
    .expect("write env");

    assert_eq!(
        crate::setup::read_env_key(&env_file, "OPENROUTER_API_KEY"),
        None
    );
    assert_eq!(
        crate::setup::read_env_key(&env_file, "MINIMAX_API_KEY"),
        None
    );
    assert_eq!(
        crate::setup::read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
        Some("real-key")
    );
}

#[test]
fn merge_missing_env_keys_skips_empty_values() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("legacy.env");
    let dst = tmp.path().join("target.env");
    std::fs::write(
        &src,
        "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='  '\nOPENAI_API_KEY=real-key\n",
    )
    .expect("write source env");

    let imported =
        crate::setup::merge_missing_env_keys(&src, &dst, "legacy.env").expect("merge env keys");
    assert_eq!(imported, 1);
    let contents = std::fs::read_to_string(&dst).expect("read merged env");
    assert!(contents.contains("OPENAI_API_KEY=real-key"));
    assert!(!contents.contains("OPENROUTER_API_KEY="));
    assert!(!contents.contains("MINIMAX_API_KEY="));
}

#[test]
fn read_env_key_handles_non_utf8_bytes_without_crashing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_file = tmp.path().join(".env");
    let mut bytes = b"OPENAI_API_KEY=real-key\nBROKEN=".to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE, 0x81, b'\n']);
    std::fs::write(&env_file, bytes).expect("write non-utf8 env");

    assert_eq!(
        crate::setup::read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
        Some("real-key")
    );
}

#[test]
fn upsert_env_key_rewrites_existing_and_appends_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_file = tmp.path().join(".env");
    std::fs::write(
        &env_file,
        "OPENAI_API_KEY=old\nHERMES_AUTH_DEFAULT_PROVIDER=openai\n",
    )
    .expect("write env");
    crate::setup::upsert_env_key(&env_file, "HERMES_AUTH_DEFAULT_PROVIDER", "nous")
        .expect("upsert");
    crate::setup::upsert_env_key(&env_file, "NOUS_API_KEY", "tok").expect("append");
    let raw = std::fs::read_to_string(&env_file).expect("read env");
    assert!(raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=nous"));
    assert!(raw.contains("NOUS_API_KEY=tok"));
    assert!(!raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=openai"));
}

#[tokio::test]
async fn profile_create_no_skills_strips_cloned_skill_overrides() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

    let source_profile = profiles_dir.join("source.yaml");
    std::fs::write(
        &source_profile,
        r#"
name: source
model: openai:gpt-4o
personality: technical
max_turns: 50
skills:
  enabled:
- contextlattice-agent-contract
  disabled:
- noisy-skill
"#,
    )
    .expect("write source profile");
    write_active_profile_name(&profiles_dir, "source").expect("set active profile");

    run_profile(
        cli,
        Some("create".to_string()),
        Some("target".to_string()),
        None,
        None,
        None,
        None,
        false,
        false,
        true,
        true,
        Some("source".to_string()),
        true,
        true,
    )
    .await
    .expect("create profile");

    let target_profile = profiles_dir.join("target.yaml");
    let parsed: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(&target_profile).expect("read target profile"),
    )
    .expect("parse target profile");
    let map = parsed.as_mapping().expect("mapping profile");
    let skills_key = serde_yaml::Value::String("skills".to_string());
    assert!(
        !map.contains_key(&skills_key),
        "skills key should be stripped"
    );
}

#[test]
fn validate_profile_name_rejects_paths() {
    let err = validate_profile_name("../danger").expect_err("should reject traversal");
    assert!(
        err.to_string().contains("path separators"),
        "unexpected error: {err}"
    );
    let err = validate_profile_name("alpha beta").expect_err("should reject spaces");
    assert!(
        err.to_string().contains("letters, numbers"),
        "unexpected error: {err}"
    );
    assert_eq!(
        validate_profile_name("prod-profile_1.2").expect("valid"),
        "prod-profile_1.2"
    );
}

#[tokio::test]
async fn profile_import_refuses_directory_clobber_target() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

    let source_profile = tmp.path().join("source.yaml");
    std::fs::write(
        &source_profile,
        r#"
name: source
model: openai:gpt-4o
personality: default
max_turns: 50
"#,
    )
    .expect("write source profile");

    let clobber_target_dir = profiles_dir.join("target.yaml");
    std::fs::create_dir_all(&clobber_target_dir).expect("create clobber directory");

    let err = run_profile(
        cli,
        Some("import".to_string()),
        Some(source_profile.to_string_lossy().into_owned()),
        None,
        None,
        Some("target".to_string()),
        None,
        false,
        true,
        false,
        false,
        None,
        true,
        false,
    )
    .await
    .expect_err("directory clobber should be rejected");

    assert!(
        err.to_string().contains("target path is a directory"),
        "unexpected error: {err}"
    );
}

#[test]
fn wecom_qr_page_url_encodes_scode() {
    let url = wecom_qr_page_url("abc/def");
    assert!(url.contains("abc%2Fdef"));
    assert!(url.starts_with("https://work.weixin.qq.com/ai/qc/gen?source=hermes&scode="));
}

#[test]
fn qqbot_connect_url_encodes_task_id() {
    let url = qqbot_connect_url("task id/+");
    assert!(url.contains("task_id=task%20id%2F%2B"));
    assert!(url.contains("source=hermes"));
}

#[test]
fn qqbot_decrypt_secret_roundtrip() {
    let key = [7u8; 32];
    let nonce = [3u8; 12];
    let key_b64 = BASE64_STANDARD.encode(key);

    let cipher = <Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key).expect("cipher init");
    let ciphertext = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), b"qq-secret".as_ref())
        .expect("encrypt");
    let mut payload = nonce.to_vec();
    payload.extend_from_slice(&ciphertext);
    let encrypted_b64 = BASE64_STANDARD.encode(payload);

    let decrypted = qqbot_decrypt_secret(&encrypted_b64, &key_b64).expect("decrypt");
    assert_eq!(decrypted, "qq-secret");
}

#[test]
fn qqbot_extract_i64_accepts_number_or_string() {
    let numeric = serde_json::json!({ "status": 2 });
    assert_eq!(qqbot_extract_i64(&numeric, &["status"]), Some(2));

    let stringified = serde_json::json!({ "status": "3" });
    assert_eq!(qqbot_extract_i64(&stringified, &["status"]), Some(3));
}

#[test]
fn query_is_local_slash_command_detects_prefixed_queries() {
    assert!(query_is_local_slash_command("/model list"));
    assert!(query_is_local_slash_command("   /graph status"));
    assert!(!query_is_local_slash_command("hello world"));
}

#[test]
fn capture_debug_log_snapshot_preserves_boundary_line() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let log_path = tmp.path().join("hermes.log");
    std::fs::write(&log_path, "line1\nline2\nline3\n").expect("write log");

    let snap = capture_debug_log_snapshot(&log_path, 1, 12);
    let full = snap.full_text.unwrap_or_default();
    assert!(full.contains("line2\nline3"));
    assert!(!full.contains("line1"));
}

#[test]
fn capture_debug_log_snapshot_caps_memory_with_long_lines() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let log_path = tmp.path().join("hermes.log");
    let long = "x".repeat(256 * 1024);
    std::fs::write(&log_path, long).expect("write long log");

    let max_bytes = 4096usize;
    let snap = capture_debug_log_snapshot(&log_path, 5, max_bytes);
    let full = snap.full_text.unwrap_or_default();
    assert!(
        full.len() <= (max_bytes * 2) + 128,
        "full snapshot should obey hard cap"
    );
}

#[test]
fn capture_debug_log_snapshot_distinguishes_missing_and_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("missing.log");
    let missing_snap = capture_debug_log_snapshot(&missing, 10, 1024);
    assert_eq!(missing_snap.tail_text, "(file not found)");

    let empty = tmp.path().join("empty.log");
    std::fs::write(&empty, "").expect("write empty log");
    let empty_snap = capture_debug_log_snapshot(&empty, 10, 1024);
    assert_eq!(empty_snap.tail_text, "(file empty)");
}

#[test]
fn sweep_expired_pending_pastes_is_best_effort_and_keeps_fresh_entries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let reports_dir = tmp.path();
    let store = debug_pending_pastes_path(reports_dir);
    let entries = vec![
        PendingPasteDelete {
            url: "https://paste.rs/expired".to_string(),
            expires_at_unix: 100,
        },
        PendingPasteDelete {
            url: "https://paste.rs/fresh".to_string(),
            expires_at_unix: 9_999_999_999,
        },
    ];
    std::fs::write(
        &store,
        serde_json::to_string_pretty(&entries).expect("serialize"),
    )
    .expect("write pending store");

    let removed = sweep_expired_pending_pastes(reports_dir, 1_000).expect("sweep");
    assert_eq!(removed, 1);

    let kept: Vec<PendingPasteDelete> =
        serde_json::from_str(&std::fs::read_to_string(&store).expect("read pending store"))
            .expect("parse pending store");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].url, "https://paste.rs/fresh");
}

#[test]
fn best_effort_sweep_handles_invalid_store_without_failing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let reports_dir = tmp.path();
    let store = debug_pending_pastes_path(reports_dir);
    std::fs::write(&store, "{invalid json").expect("write invalid json");

    let removed = best_effort_sweep_expired_pending_pastes(reports_dir, 1_000);
    assert_eq!(removed, 0);
}

#[test]
fn run_sessions_db_auto_maintenance_degrades_when_home_is_invalid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bad_home = tmp.path().join("not-a-dir");
    std::fs::write(&bad_home, "x").expect("write blocker file");

    let mut cfg = hermes_config::GatewayConfig::default();
    cfg.home_dir = Some(bad_home.to_string_lossy().to_string());
    cfg.sessions.auto_prune = true;

    let result = std::panic::catch_unwind(|| run_sessions_db_auto_maintenance(&cfg));
    assert!(
        result.is_ok(),
        "maintenance should degrade without panicking"
    );
}

#[test]
fn gateway_auth_provider_keys_include_primary_platforms() {
    for key in ["telegram", "weixin", "discord", "slack"] {
        let mapped = gateway_platform_provider_key(key);
        if key == "telegram" || key == "weixin" {
            assert!(mapped.is_none(), "{key} handled by dedicated auth flow");
        } else {
            assert_eq!(mapped, Some(key));
        }
    }
}

#[test]
fn gateway_requirement_check_flags_missing_required_fields() {
    let mut config = hermes_config::GatewayConfig::default();
    config
        .platforms
        .insert("telegram".to_string(), make_platform(true, None));
    config
        .platforms
        .insert("qqbot".to_string(), make_platform(true, None));
    let issues = gateway_requirement_issues(&config);
    assert!(issues.iter().any(|s| s.contains("telegram")));
    assert!(issues.iter().any(|s| s.contains("qqbot")));
}

#[test]
fn gateway_requirement_check_accepts_complete_qqbot_and_wecom_callback() {
    let mut config = hermes_config::GatewayConfig::default();

    let mut qqbot = make_platform(true, None);
    qqbot
        .extra
        .insert("app_id".to_string(), serde_json::json!("qq-app"));
    qqbot
        .extra
        .insert("client_secret".to_string(), serde_json::json!("qq-secret"));
    config.platforms.insert("qqbot".to_string(), qqbot);

    let mut wecom_cb = make_platform(true, Some("cb-token"));
    wecom_cb
        .extra
        .insert("corp_id".to_string(), serde_json::json!("wwcorp"));
    wecom_cb
        .extra
        .insert("corp_secret".to_string(), serde_json::json!("corp-secret"));
    wecom_cb
        .extra
        .insert("agent_id".to_string(), serde_json::json!("1000002"));
    wecom_cb.extra.insert(
        "encoding_aes_key".to_string(),
        serde_json::json!("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
    );
    config
        .platforms
        .insert("wecom_callback".to_string(), wecom_cb);

    assert!(gateway_requirement_issues(&config).is_empty());
}

#[tokio::test]
async fn register_gateway_adapters_registers_primary_platforms_when_config_is_complete() {
    let mut config = hermes_config::GatewayConfig::default();

    let mut telegram = make_platform(true, Some("tg-token"));
    telegram
        .extra
        .insert("polling".to_string(), serde_json::json!(false));
    config.platforms.insert("telegram".to_string(), telegram);

    let mut weixin = make_platform(true, Some("wx-token"));
    weixin
        .extra
        .insert("account_id".to_string(), serde_json::json!("wxid_abc"));
    config.platforms.insert("weixin".to_string(), weixin);

    config.platforms.insert(
        "discord".to_string(),
        make_platform(true, Some("discord-token")),
    );
    config
        .platforms
        .insert("slack".to_string(), make_platform(true, Some("xoxb-slack")));

    let gateway = make_gateway();
    let mut sidecar_tasks = Vec::new();
    register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
        .await
        .expect("primary platform registration should succeed");

    let mut names = gateway.adapter_names().await;
    names.sort();
    assert!(names.contains(&"telegram".to_string()));
    assert!(names.contains(&"weixin".to_string()));
    assert!(names.contains(&"discord".to_string()));
    assert!(names.contains(&"slack".to_string()));

    for task in sidecar_tasks {
        task.abort();
    }
}

#[tokio::test]
async fn register_gateway_adapters_skips_primary_platforms_when_required_credentials_missing() {
    let mut config = hermes_config::GatewayConfig::default();
    config
        .platforms
        .insert("telegram".to_string(), make_platform(true, None));
    config
        .platforms
        .insert("weixin".to_string(), make_platform(true, None));
    config
        .platforms
        .insert("discord".to_string(), make_platform(true, None));
    config
        .platforms
        .insert("slack".to_string(), make_platform(true, None));

    let gateway = make_gateway();
    let mut sidecar_tasks = Vec::new();
    register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
        .await
        .expect("missing credentials should be handled gracefully");

    assert!(
        gateway.adapter_names().await.is_empty(),
        "no primary adapter should register when required credentials are missing"
    );
    for task in sidecar_tasks {
        task.abort();
    }
}

#[tokio::test]
async fn register_gateway_adapters_registers_qqbot_and_wecom_callback() {
    let mut config = hermes_config::GatewayConfig::default();

    let mut qqbot = make_platform(true, None);
    qqbot
        .extra
        .insert("app_id".to_string(), serde_json::json!("qq-app"));
    qqbot
        .extra
        .insert("client_secret".to_string(), serde_json::json!("qq-secret"));
    config.platforms.insert("qqbot".to_string(), qqbot);

    let mut wecom_cb = make_platform(true, None);
    wecom_cb
        .extra
        .insert("corp_id".to_string(), serde_json::json!("wwcorp"));
    wecom_cb
        .extra
        .insert("corp_secret".to_string(), serde_json::json!("corp-secret"));
    wecom_cb
        .extra
        .insert("agent_id".to_string(), serde_json::json!("1000002"));
    wecom_cb
        .extra
        .insert("token".to_string(), serde_json::json!("cb-token"));
    wecom_cb.extra.insert(
        "encoding_aes_key".to_string(),
        serde_json::json!("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
    );
    config
        .platforms
        .insert("wecom_callback".to_string(), wecom_cb);

    let gateway = make_gateway();
    let mut sidecar_tasks = Vec::new();
    register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
        .await
        .expect("qqbot and wecom_callback should register");

    let names = gateway.adapter_names().await;
    assert!(names.contains(&"qqbot".to_string()));
    assert!(names.contains(&"wecom_callback".to_string()));
}

#[test]
fn doctor_self_heal_creates_missing_state_dirs() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let state_root = hermes_state_root(&cli);
    assert!(!state_root.join("profiles").exists());

    let actions = run_doctor_self_heal(&cli);
    assert!(state_root.join("profiles").exists());
    assert!(state_root.join("sessions").exists());
    assert!(state_root.join("logs").exists());
    assert!(
        actions
            .iter()
            .any(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("created"))
    );
}

#[test]
fn doctor_self_heal_removes_stale_gateway_pid_file() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let pid_path = CliStateRoot::from_state_root(&hermes_state_root(&cli)).gateway_pid();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir pid dir");
    }
    std::fs::write(&pid_path, "999999").expect("write stale pid");
    assert!(pid_path.exists());

    let actions = run_doctor_self_heal(&cli);
    assert!(!pid_path.exists(), "stale pid file should be removed");
    assert!(actions.iter().any(|entry| {
        entry
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("removed stale gateway pid file")
    }));
}

#[test]
fn doctor_elite_diagnostics_payload_has_required_sections() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let payload = build_elite_doctor_diagnostics(&cli);
    assert!(payload.get("provenance").is_some());
    assert!(payload.get("route_learning").is_some());
    assert!(payload.get("route_health").is_some());
    assert!(payload.get("tool_policy").is_some());
    assert!(payload.get("elite_gate").is_some());
}

#[test]
fn replay_integrity_detects_chain_break() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let replay = tmp.path().join("session.jsonl");
    std::fs::write(
        &replay,
        r#"{"seq":1,"event":"a","prev_hash":"seed","event_hash":"h1","payload":{"ok":true}}
{"seq":2,"event":"b","prev_hash":"BROKEN","event_hash":"h2","payload":{"ok":true}}
"#,
    )
    .expect("write replay");

    let summary = replay_integrity_for_file(&replay);
    assert_eq!(summary.events, 2);
    assert!(!summary.hash_chain_ok);
}

#[test]
fn replay_manifest_aggregates_counts() {
    let items = vec![
        ReplayIntegritySummary {
            file: "a.jsonl".to_string(),
            checksum_sha256: Some("abc".to_string()),
            events: 3,
            invalid_lines: 0,
            hash_chain_ok: true,
            last_event_hash: Some("h1".to_string()),
        },
        ReplayIntegritySummary {
            file: "b.jsonl".to_string(),
            checksum_sha256: Some("def".to_string()),
            events: 2,
            invalid_lines: 1,
            hash_chain_ok: false,
            last_event_hash: Some("h2".to_string()),
        },
    ];
    let manifest = replay_manifest_json(&items);
    assert_eq!(manifest["totals"]["files"], 2);
    assert_eq!(manifest["totals"]["events"], 5);
    assert_eq!(manifest["totals"]["invalid_lines"], 1);
    assert_eq!(manifest["totals"]["hash_chain_ok"], false);
}
