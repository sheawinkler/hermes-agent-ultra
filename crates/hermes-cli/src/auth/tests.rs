use super::*;
use crate::test_env_lock;

fn nous_test_jwt(seconds: i64, scope: Value) -> String {
    let header = serde_json::json!({ "alg": "none", "typ": "JWT" });
    let claims = serde_json::json!({
        "exp": Utc::now().timestamp() + seconds,
        "scope": scope,
    });
    format!(
        "{}.{}.sig",
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("header json")),
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims json"))
    )
}

fn nous_test_state(access_token: String) -> NousAuthState {
    NousAuthState {
        portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
        inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
        client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
        scope: Some(DEFAULT_NOUS_SCOPE.to_string()),
        token_type: "Bearer".to_string(),
        access_token,
        refresh_token: Some("refresh".to_string()),
        obtained_at: Utc::now().to_rfc3339(),
        expires_at: None,
        expires_in: None,
        agent_key: None,
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: None,
        agent_key_obtained_at: None,
    }
}

#[test]
fn nous_runtime_api_key_prefers_agent_key() {
    let state = NousAuthState {
        portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
        inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
        client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
        scope: Some(DEFAULT_NOUS_SCOPE.to_string()),
        token_type: "Bearer".to_string(),
        access_token: "portal-access".to_string(),
        refresh_token: Some("refresh".to_string()),
        obtained_at: Utc::now().to_rfc3339(),
        expires_at: None,
        expires_in: None,
        agent_key: Some("agent-key".to_string()),
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: None,
        agent_key_obtained_at: None,
    };
    assert_eq!(state.runtime_api_key().as_deref(), Some("agent-key"));
}

#[test]
fn nous_timestamp_is_expiring_treats_missing_as_expiring() {
    assert!(timestamp_is_expiring(None, 120));
    assert!(timestamp_is_expiring(Some(""), 120));
    assert!(timestamp_is_expiring(Some("not-a-date"), 120));
}

#[test]
fn nous_invoke_jwt_usable_uses_invoke_scope_and_jwt_expiry() {
    let token = nous_test_jwt(900, Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string()));
    assert!(nous_invoke_jwt_is_usable(
        &token,
        Some(DEFAULT_NOUS_SCOPE),
        None,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    ));
    assert_eq!(
        nous_invoke_jwt_status(
            &nous_test_jwt(900, Value::String("profile".to_string())),
            None,
            None,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        ),
        Some("missing_inference_invoke_scope")
    );
    assert_eq!(
        nous_invoke_jwt_status(
            &nous_test_jwt(30, Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string())),
            None,
            None,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        ),
        Some("invoke_jwt_expiring")
    );
}

#[test]
fn nous_invoke_jwt_selection_mirrors_access_token_to_agent_key_fields() {
    let token = nous_test_jwt(900, serde_json::json!([NOUS_INFERENCE_INVOKE_SCOPE]));
    let mut state = nous_test_state(token.clone());
    state.expires_at = Some("2000-01-01T00:00:00Z".to_string());
    state.agent_key = Some("legacy-opaque-agent-key".to_string());
    state.agent_key_expires_at = Some("2099-01-01T00:00:00Z".to_string());

    assert_nous_invoke_jwt_usable(&state, None, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS)
        .expect("invoke jwt usable");
    set_nous_agent_key_from_invoke_jwt(&mut state);

    assert_eq!(state.agent_key.as_deref(), Some(token.as_str()));
    assert_eq!(state.agent_key_id, None);
    assert_eq!(state.agent_key_reused, Some(false));
    let mirrored_expiry = state
        .agent_key_expires_at
        .as_deref()
        .and_then(parse_iso_timestamp_utc)
        .expect("jwt expiry mirrored");
    assert!(mirrored_expiry > Utc::now() + chrono::Duration::seconds(600));
    assert_eq!(state.expires_at, state.agent_key_expires_at);
}

#[tokio::test]
async fn resolve_nous_runtime_credentials_selects_invoke_jwt_without_mint() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let token = nous_test_jwt(900, Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string()));
    let mut state = nous_test_state(token.clone());
    state.agent_key = Some("legacy-opaque-agent-key".to_string());
    state.agent_key_expires_at = Some("2099-01-01T00:00:00Z".to_string());
    save_nous_auth_state(&state).expect("save nous state");

    let resolved = resolve_nous_runtime_credentials(
        false,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    .expect("resolve nous runtime credentials");
    assert_eq!(resolved.api_key, token);
    assert_eq!(resolved.source, NOUS_AUTH_PATH_INVOKE_JWT);
    assert!(resolved.expires_in.unwrap_or_default() > 600);

    let saved = read_provider_auth_state("nous")
        .expect("read saved state")
        .expect("saved state");
    assert_eq!(
        saved.get("agent_key").and_then(Value::as_str),
        Some(resolved.api_key.as_str())
    );
    assert!(
        saved.get("agent_key_id").is_none(),
        "invoke JWT path should not preserve legacy agent key id"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[tokio::test]
async fn resolve_nous_runtime_credentials_rejects_missing_invoke_scope() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let token = nous_test_jwt(900, Value::String("profile".to_string()));
    let mut state = nous_test_state(token);
    state.scope = Some("profile".to_string());
    state.refresh_token = None;
    save_nous_auth_state(&state).expect("save nous state");

    let err = resolve_nous_runtime_credentials(
        false,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    .expect_err("missing invoke scope should fail");
    assert!(
        err.to_string().contains("missing_inference_invoke_scope"),
        "unexpected error: {err}"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[test]
fn clear_provider_auth_state_is_noop_when_missing() {
    let provider = format!("missing-{}", uuid::Uuid::new_v4().simple());
    let removed = clear_provider_auth_state(&provider).expect("clear");
    assert!(!removed);
}

#[test]
fn codex_oauth_numeric_fields_accept_number_or_string() {
    let device_from_number: CodexDeviceUserCodeResponse =
        serde_json::from_value(serde_json::json!({
            "user_code": "ABCD-EFGH",
            "device_auth_id": "device-auth-id",
            "interval": 5
        }))
        .expect("numeric interval");
    assert_eq!(device_from_number.interval, Some(5));

    let device_from_string: CodexDeviceUserCodeResponse =
        serde_json::from_value(serde_json::json!({
            "user_code": "ABCD-EFGH",
            "device_auth_id": "device-auth-id",
            "interval": "5"
        }))
        .expect("string interval");
    assert_eq!(device_from_string.interval, Some(5));

    let token_from_string: CodexTokenResponse = serde_json::from_value(serde_json::json!({
        "access_token": "access",
        "refresh_token": "refresh",
        "expires_in": "3600"
    }))
    .expect("string expires_in");
    assert_eq!(token_from_string.expires_in, Some(3600));
}

#[test]
fn discover_existing_openai_oauth_reads_env_path() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("codex-auth.json");
    let exp = Utc::now().timestamp() + 3600;
    let payload = serde_json::json!({ "exp": exp });
    let payload_b64 =
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload json"));
    let id_token = format!("header.{}.sig", payload_b64);
    let raw = serde_json::json!({
        "auth_mode": "chatgpt",
        "last_refresh": "2026-04-27T00:00:00Z",
        "tokens": {
            "access_token": "openai-access",
            "refresh_token": "openai-refresh",
            "id_token": id_token,
        }
    });
    std::fs::write(
        &auth_path,
        serde_json::to_string_pretty(&raw).expect("serialize auth fixture"),
    )
    .expect("write auth fixture");
    std::env::set_var(
        "HERMES_OPENAI_OAUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );

    let imported = discover_existing_openai_oauth()
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, auth_path);
    assert_eq!(imported.state.tokens.access_token, "openai-access");
    assert_eq!(
        imported.state.tokens.refresh_token.as_deref(),
        Some("openai-refresh")
    );
    assert_eq!(imported.state.base_url, DEFAULT_CODEX_BASE_URL.to_string());
    assert_eq!(imported.state.auth_mode.as_deref(), Some("chatgpt"));
    assert!(
        imported.state.tokens.expires_in.unwrap_or_default() > 100,
        "expected imported token TTL from id_token exp"
    );
    std::env::remove_var("HERMES_OPENAI_OAUTH_FILE");
}

#[test]
fn discover_existing_openai_codex_oauth_reads_env_path() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("codex-auth.json");
    let exp = Utc::now().timestamp() + 3600;
    let payload = serde_json::json!({ "exp": exp });
    let payload_b64 =
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload json"));
    let id_token = format!("header.{}.sig", payload_b64);
    let raw = serde_json::json!({
        "auth_mode": "chatgpt",
        "last_refresh": "2026-04-27T00:00:00Z",
        "tokens": {
            "access_token": "codex-access",
            "refresh_token": "codex-refresh",
            "id_token": id_token,
        }
    });
    std::fs::write(
        &auth_path,
        serde_json::to_string_pretty(&raw).expect("serialize auth fixture"),
    )
    .expect("write auth fixture");
    std::env::set_var(
        "HERMES_OPENAI_CODEX_OAUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );

    let imported = discover_existing_openai_codex_oauth()
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, auth_path);
    assert_eq!(imported.state.tokens.access_token, "codex-access");
    assert_eq!(
        imported.state.tokens.refresh_token.as_deref(),
        Some("codex-refresh")
    );
    assert_eq!(imported.state.base_url, DEFAULT_CODEX_BASE_URL.to_string());
    assert_eq!(imported.state.auth_mode.as_deref(), Some("chatgpt"));
    std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_FILE");
}

#[test]
fn discover_existing_anthropic_oauth_reads_claude_credentials_file() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let cred_path = tmp.path().join(".credentials.json");
    let expires_at = Utc::now().timestamp_millis() + 3600_000;
    let raw = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "ant-access",
            "refreshToken": "ant-refresh",
            "expiresAt": expires_at,
        }
    });
    std::fs::write(
        &cred_path,
        serde_json::to_string_pretty(&raw).expect("serialize credentials"),
    )
    .expect("write credentials");
    std::env::set_var(
        "CLAUDE_CODE_CREDENTIALS_FILE",
        cred_path.to_string_lossy().to_string(),
    );

    let imported = discover_existing_anthropic_oauth_with_keychain(None)
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, cred_path);
    assert_eq!(imported.source, "claude_code_credentials_file");
    assert_eq!(imported.state.access_token, "ant-access");
    assert_eq!(imported.state.refresh_token.as_deref(), Some("ant-refresh"));
    assert_eq!(imported.state.expires_at_ms, Some(expires_at));
    std::env::remove_var("CLAUDE_CODE_CREDENTIALS_FILE");
}

#[test]
fn anthropic_oauth_keychain_payload_parses_valid_claude_code_entry() {
    let raw = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "keychain-access",
            "refreshToken": "keychain-refresh",
            "expiresAt": 9_999_999_999_999i64,
        }
    });
    let imported = load_anthropic_oauth_import_from_keychain_payload(
        &serde_json::to_string(&raw).expect("serialize keychain payload"),
    )
    .expect("imported");

    assert_eq!(imported.source_path, anthropic_keychain_source_path());
    assert_eq!(imported.source, "macos_keychain");
    assert_eq!(imported.state.access_token, "keychain-access");
    assert_eq!(
        imported.state.refresh_token.as_deref(),
        Some("keychain-refresh")
    );
    assert_eq!(imported.state.expires_at_ms, Some(9_999_999_999_999));
}

#[test]
fn anthropic_oauth_keychain_payload_rejects_invalid_entries() {
    assert!(load_anthropic_oauth_import_from_keychain_payload("").is_none());
    assert!(load_anthropic_oauth_import_from_keychain_payload("not json").is_none());
    assert!(load_anthropic_oauth_import_from_keychain_payload(
        r#"{"someOtherService":{"accessToken":"tok"}}"#
    )
    .is_none());
    assert!(load_anthropic_oauth_import_from_keychain_payload(
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"refresh"}}"#
    )
    .is_none());
}

#[test]
fn discover_existing_anthropic_oauth_prefers_keychain_over_json_file() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let cred_path = tmp.path().join(".credentials.json");
    let raw = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "json-access",
            "refreshToken": "json-refresh",
            "expiresAt": 9_999_999_999_999i64,
        }
    });
    std::fs::write(
        &cred_path,
        serde_json::to_string_pretty(&raw).expect("serialize credentials"),
    )
    .expect("write credentials");
    std::env::set_var(
        "CLAUDE_CODE_CREDENTIALS_FILE",
        cred_path.to_string_lossy().to_string(),
    );

    let keychain_import = AnthropicOAuthImport {
        state: AnthropicOAuthState {
            access_token: "keychain-access".to_string(),
            refresh_token: Some("keychain-refresh".to_string()),
            expires_at_ms: Some(9_999_999_999_999),
        },
        source_path: anthropic_keychain_source_path(),
        source: "macos_keychain".to_string(),
    };
    let imported = discover_existing_anthropic_oauth_with_keychain(Some(keychain_import))
        .expect("discover")
        .expect("imported");

    assert_eq!(imported.state.access_token, "keychain-access");
    assert_eq!(imported.source, "macos_keychain");
    std::env::remove_var("CLAUDE_CODE_CREDENTIALS_FILE");
}

#[test]
fn discover_existing_nous_oauth_reads_auth_store_provider_state() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_store = tmp.path().join("auth.json");
    let raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "portal_base_url": DEFAULT_NOUS_PORTAL_URL,
                "inference_base_url": DEFAULT_NOUS_INFERENCE_URL,
                "client_id": DEFAULT_NOUS_CLIENT_ID,
                "scope": DEFAULT_NOUS_SCOPE,
                "token_type": "Bearer",
                "access_token": "nous-access",
                "refresh_token": "nous-refresh",
                "obtained_at": "2026-04-27T00:00:00Z",
                "agent_key": "nous-agent-key"
            }
        }
    });
    std::fs::write(
        &auth_store,
        serde_json::to_string_pretty(&raw).expect("serialize auth store"),
    )
    .expect("write auth store");
    std::env::set_var("HERMES_AUTH_FILE", auth_store.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let imported = discover_existing_nous_oauth()
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, auth_store);
    assert_eq!(imported.state.access_token, "nous-access");
    assert_eq!(
        imported.state.refresh_token.as_deref(),
        Some("nous-refresh")
    );
    assert_eq!(
        imported.state.runtime_api_key().as_deref(),
        Some("nous-agent-key")
    );
    std::env::remove_var("HERMES_AUTH_FILE");
}

#[test]
fn read_provider_auth_state_falls_back_to_global_store_when_primary_missing_provider() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var("HOME").ok();
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();

    std::env::set_var("HOME", tmp.path());
    std::env::remove_var("HERMES_HOME");

    let primary_store = tmp.path().join("profile-auth.json");
    let primary_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "openai": { "access_token": "primary-openai" }
        }
    });
    std::fs::write(
        &primary_store,
        serde_json::to_string_pretty(&primary_raw).expect("serialize primary auth"),
    )
    .expect("write primary auth");
    std::env::set_var(
        "HERMES_AUTH_FILE",
        primary_store.to_string_lossy().to_string(),
    );

    let fallback_store = tmp.path().join(".hermes-agent-ultra").join("auth.json");
    std::fs::create_dir_all(
        fallback_store
            .parent()
            .expect("fallback store should have parent"),
    )
    .expect("mkdir fallback parent");
    let fallback_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "access_token": "fallback-nous-access"
            }
        }
    });
    std::fs::write(
        &fallback_store,
        serde_json::to_string_pretty(&fallback_raw).expect("serialize fallback auth"),
    )
    .expect("write fallback auth");

    let found = read_provider_auth_state("nous")
        .expect("read provider auth state")
        .expect("fallback provider should resolve");
    assert_eq!(
        found
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "fallback-nous-access"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn read_valid_nous_auth_state_ignores_malformed_fallback_store() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var("HOME").ok();
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();

    std::env::set_var("HOME", tmp.path());
    std::env::remove_var("HERMES_HOME");

    let primary_store = tmp.path().join("profile-auth.json");
    let primary_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "openai": { "access_token": "primary-openai" }
        }
    });
    std::fs::write(
        &primary_store,
        serde_json::to_string_pretty(&primary_raw).expect("serialize primary auth"),
    )
    .expect("write primary auth");
    std::env::set_var(
        "HERMES_AUTH_FILE",
        primary_store.to_string_lossy().to_string(),
    );
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let fallback_store = tmp.path().join(".hermes").join("auth.json");
    std::fs::create_dir_all(
        fallback_store
            .parent()
            .expect("fallback store should have parent"),
    )
    .expect("mkdir fallback parent");
    let fallback_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "client_id": DEFAULT_NOUS_CLIENT_ID,
                "inference_base_url": DEFAULT_NOUS_INFERENCE_URL,
                "last_auth_error": "unauthorized",
                "portal_base_url": DEFAULT_NOUS_PORTAL_URL,
                "scope": DEFAULT_NOUS_SCOPE,
                "token_type": "Bearer"
            }
        }
    });
    std::fs::write(
        &fallback_store,
        serde_json::to_string_pretty(&fallback_raw).expect("serialize fallback auth"),
    )
    .expect("write fallback auth");

    assert!(
        read_provider_auth_state("nous")
            .expect("read raw provider auth state")
            .is_some(),
        "raw fallback store should still be discoverable"
    );
    assert!(
        read_valid_nous_auth_state()
            .expect("read valid nous auth state")
            .is_none(),
        "malformed fallback state must not count as a valid Nous login"
    );

    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn read_valid_nous_auth_state_rejects_profile_only_access_state_without_refresh() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let mut state = nous_test_state("profile-access-token".to_string());
    state.scope = Some("profile".to_string());
    state.refresh_token = None;
    save_nous_auth_state(&state).expect("save nous state");

    assert!(
        read_valid_nous_auth_state()
            .expect("read valid nous auth state")
            .is_none(),
        "profile-only access token without refresh must not count as runtime-valid"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[test]
fn read_valid_nous_auth_state_accepts_refreshable_access_state() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let mut state = nous_test_state("profile-access-token".to_string());
    state.scope = Some("profile".to_string());
    state.refresh_token = Some("refresh".to_string());
    save_nous_auth_state(&state).expect("save nous state");

    let found = read_valid_nous_auth_state()
        .expect("read valid nous auth state")
        .expect("refreshable state should count as valid");
    assert_eq!(found.refresh_token.as_deref(), Some("refresh"));

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[tokio::test]
async fn resolve_nous_runtime_credentials_ignores_malformed_fallback_store() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var("HOME").ok();
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();

    std::env::set_var("HOME", tmp.path());
    std::env::remove_var("HERMES_HOME");

    let primary_store = tmp.path().join("profile-auth.json");
    let primary_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "openai": { "access_token": "primary-openai" }
        }
    });
    std::fs::write(
        &primary_store,
        serde_json::to_string_pretty(&primary_raw).expect("serialize primary auth"),
    )
    .expect("write primary auth");
    std::env::set_var(
        "HERMES_AUTH_FILE",
        primary_store.to_string_lossy().to_string(),
    );
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let fallback_store = tmp.path().join(".hermes").join("auth.json");
    std::fs::create_dir_all(
        fallback_store
            .parent()
            .expect("fallback store should have parent"),
    )
    .expect("mkdir fallback parent");
    let fallback_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "client_id": DEFAULT_NOUS_CLIENT_ID,
                "inference_base_url": DEFAULT_NOUS_INFERENCE_URL,
                "last_auth_error": "unauthorized",
                "portal_base_url": DEFAULT_NOUS_PORTAL_URL,
                "scope": DEFAULT_NOUS_SCOPE,
                "token_type": "Bearer"
            }
        }
    });
    std::fs::write(
        &fallback_store,
        serde_json::to_string_pretty(&fallback_raw).expect("serialize fallback auth"),
    )
    .expect("write fallback auth");

    let err = resolve_nous_runtime_credentials(
        false,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    .expect_err("malformed fallback store should not resolve runtime credentials");
    assert!(
        err.to_string()
            .contains("Hermes is not logged into Nous Portal"),
        "unexpected error: {err}"
    );

    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn auth_store_write_respects_auth_file_override() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let override_path = tmp.path().join("override").join("auth.json");
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    std::env::set_var("HERMES_HOME", &home);
    std::env::set_var("HERMES_AUTH_FILE", &override_path);

    let path = save_provider_auth_state(
        "nous",
        serde_json::json!({
            "access_token": "override-access-token",
            "agent_key": "override-agent-key"
        }),
    )
    .expect("save auth state");

    assert_eq!(path, override_path);
    assert!(
        override_path.exists(),
        "override auth file should be written"
    );
    assert!(
        !home.join("auth.json").exists(),
        "HERMES_AUTH_FILE writes must not touch HERMES_HOME/auth.json"
    );
    let saved = read_provider_auth_state("nous")
        .expect("read auth state")
        .expect("provider state");
    assert_eq!(
        saved.get("agent_key").and_then(Value::as_str),
        Some("override-agent-key")
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
}

#[test]
fn auth_store_write_is_atomic_owner_only_and_cleans_tmp() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    std::env::set_var("HERMES_HOME", tmp.path());
    std::env::remove_var("HERMES_AUTH_FILE");

    let path = save_provider_auth_state(
        "openai",
        serde_json::json!({
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token"
        }),
    )
    .expect("save auth state");

    let raw = std::fs::read_to_string(&path).expect("read auth store");
    assert!(raw.contains("test-access-token"));
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("read temp home")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary auth files should be cleaned up: {leftovers:?}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("auth metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "auth store should be owner-only");
    }

    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
}

#[test]
fn qwen_access_token_is_expiring_honors_skew() {
    let now_ms = Utc::now().timestamp_millis();
    assert!(qwen_access_token_is_expiring(None, 120));
    assert!(qwen_access_token_is_expiring(Some(now_ms + 30_000), 120));
    assert!(!qwen_access_token_is_expiring(Some(now_ms + 300_000), 120));
}

#[test]
fn gemini_packed_refresh_roundtrip() {
    let packed =
        pack_gemini_refresh(Some("r1"), Some("proj"), Some("managed")).expect("packed refresh");
    assert_eq!(packed, "r1|proj|managed");
    let parsed = parse_packed_gemini_refresh(Some(&packed));
    assert_eq!(parsed.0.as_deref(), Some("r1"));
    assert_eq!(parsed.1.as_deref(), Some("proj"));
    assert_eq!(parsed.2.as_deref(), Some("managed"));
}

#[test]
fn gemini_access_token_is_expiring_honors_skew() {
    let now_ms = Utc::now().timestamp_millis();
    assert!(gemini_access_token_is_expiring(None, 60));
    assert!(gemini_access_token_is_expiring(Some(now_ms + 1_000), 60));
    assert!(!gemini_access_token_is_expiring(Some(now_ms + 120_000), 60));
}

#[test]
fn gemini_state_read_write_roundtrip() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("google_oauth.json");
    std::env::set_var(
        "HERMES_GEMINI_OAUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );
    let state = GeminiOAuthFileState {
        refresh: Some("refresh-token|proj-1|managed-1".to_string()),
        access: Some("access-token".to_string()),
        expires: Some(Utc::now().timestamp_millis() + 5 * 60 * 1000),
        email: Some("dev@example.com".to_string()),
        project_id: Some("proj-1".to_string()),
        managed_project_id: Some("managed-1".to_string()),
    };
    save_gemini_cli_state(&state).expect("save");
    let loaded = read_gemini_cli_state().expect("read");
    assert_eq!(loaded.access.as_deref(), Some("access-token"));
    assert_eq!(
        parse_packed_gemini_refresh(loaded.refresh.as_deref())
            .0
            .as_deref(),
        Some("refresh-token")
    );
    assert_eq!(loaded.project_id.as_deref(), Some("proj-1"));
    assert_eq!(loaded.managed_project_id.as_deref(), Some("managed-1"));
    std::env::remove_var("HERMES_GEMINI_OAUTH_FILE");
}

#[tokio::test]
async fn resolve_qwen_runtime_credentials_reads_qwen_cli_auth_file() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("oauth_creds.json");
    let expiry_date = Utc::now().timestamp_millis() + 5 * 60 * 1000;
    let payload = serde_json::json!({
        "access_token": "qwen-access-token",
        "refresh_token": "qwen-refresh-token",
        "token_type": "Bearer",
        "resource_url": "portal.qwen.ai",
        "expiry_date": expiry_date,
    });
    std::fs::write(&auth_path, serde_json::to_string_pretty(&payload).unwrap())
        .expect("write auth file");
    std::env::set_var(
        "HERMES_QWEN_CLI_AUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );
    std::env::set_var("HERMES_QWEN_BASE_URL", "https://portal.qwen.ai/v1");

    let resolved = resolve_qwen_runtime_credentials(false, false, 120)
        .await
        .expect("resolve");
    assert_eq!(resolved.provider, "qwen-oauth");
    assert_eq!(resolved.api_key, "qwen-access-token");
    assert_eq!(resolved.base_url, "https://portal.qwen.ai/v1".to_string());
    assert_eq!(resolved.expires_at_ms, Some(expiry_date));
    assert_eq!(
        resolved.refresh_token.as_deref(),
        Some("qwen-refresh-token")
    );

    std::env::remove_var("HERMES_QWEN_CLI_AUTH_FILE");
    std::env::remove_var("HERMES_QWEN_BASE_URL");
}
