use super::*;

#[test]
fn read_env_key_treats_empty_values_as_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_file = tmp.path().join(".env");
    std::fs::write(
        &env_file,
        "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='   '\nOPENAI_API_KEY=real-key\n",
    )
    .expect("write env");

    assert_eq!(read_env_key(&env_file, "OPENROUTER_API_KEY"), None);
    assert_eq!(read_env_key(&env_file, "MINIMAX_API_KEY"), None);
    assert_eq!(
        read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
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

    let imported = merge_missing_env_keys(&src, &dst, "legacy.env").expect("merge env keys");
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
        read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
        Some("real-key")
    );
}

#[test]
fn provenance_sign_and_verify_round_trip() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    let body = b"{\"ok\":true}";
    std::fs::write(&artifact, body).expect("write artifact");

    let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
    let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");
    let verified =
        verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
    assert!(verified.ok, "verification should pass");
    assert_eq!(verified.code, "ok");
    assert!(verified.reason.is_none(), "no reason on success");
}

#[test]
fn provenance_verify_detects_tampered_artifact() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    let body = b"{\"ok\":true}";
    std::fs::write(&artifact, body).expect("write artifact");
    let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
    let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

    std::fs::write(&artifact, b"{\"ok\":false}").expect("tamper artifact");

    let verified =
        verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
    assert!(!verified.ok, "tamper must fail");
    assert_eq!(verified.code, "artifact_sha256_mismatch");
    assert_eq!(verified.reason.as_deref(), Some("artifact_sha256 mismatch"));
}

#[test]
fn provenance_verify_detects_signature_mismatch() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    let body = b"{\"ok\":true}";
    std::fs::write(&artifact, body).expect("write artifact");
    let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
    let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

    let mut parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("read sidecar"))
            .expect("parse sidecar");
    parsed["signature_hex"] = serde_json::json!("deadbeef");
    std::fs::write(
        &sidecar,
        serde_json::to_string_pretty(&parsed).expect("serialize sidecar"),
    )
    .expect("write tampered sidecar");

    let verified =
        verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
    assert!(!verified.ok, "signature mismatch must fail");
    assert_eq!(verified.code, "signature_mismatch");
    assert_eq!(verified.reason.as_deref(), Some("signature mismatch"));
}

#[test]
fn provenance_verify_detects_missing_sidecar_with_code() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let artifact = tmp.path().join("doctor-snapshot.json");
    std::fs::write(&artifact, b"{\"ok\":true}").expect("write artifact");

    let verified = verify_artifact_provenance(&cli, &artifact, None).expect("verify");
    assert!(!verified.ok, "missing sidecar must fail");
    assert_eq!(verified.code, "signature_read_error");
    assert!(verified
        .reason
        .as_deref()
        .unwrap_or("")
        .contains(".sig.json"));
}

#[tokio::test]
async fn rotate_provenance_key_archives_previous_key_and_rekeys() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("create cfg dir");
    let cli = Cli::parse_from([
        "hermes-agent-ultra",
        "--config-dir",
        config_dir.to_str().expect("cfg path utf8"),
    ]);

    let old_key = load_or_create_provenance_key(&cli, true).expect("create key");
    run_rotate_provenance_key(cli.clone(), true)
        .await
        .expect("rotate key");
    let new_key = load_or_create_provenance_key(&cli, false).expect("load rotated key");
    assert_ne!(old_key, new_key, "rotation must change active key bytes");

    let auth_dir = provenance_key_path_for_cli(&cli)
        .parent()
        .expect("key path parent")
        .to_path_buf();
    let archived_count = std::fs::read_dir(auth_dir)
        .expect("read auth dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("provenance.key.")
                && entry.file_name().to_string_lossy().ends_with(".bak")
        })
        .count();
    assert!(archived_count >= 1, "rotation should archive previous key");
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
    upsert_env_key(&env_file, "HERMES_AUTH_DEFAULT_PROVIDER", "nous").expect("upsert");
    upsert_env_key(&env_file, "NOUS_API_KEY", "tok").expect("append");
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

#[tokio::test]
async fn profile_create_clone_from_implies_config_clone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

    let source_profile = profiles_dir.join("coder.yaml");
    std::fs::write(
        &source_profile,
        r#"
name: coder
model: anthropic/claude-sonnet-4
personality: focused
max_turns: 77
"#,
    )
    .expect("write source profile");

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
        false,
        false,
        Some("coder".to_string()),
        true,
        false,
    )
    .await
    .expect("create profile");

    let target_profile = profiles_dir.join("target.yaml");
    let parsed: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(&target_profile).expect("read target profile"),
    )
    .expect("parse target profile");
    let map = parsed.as_mapping().expect("mapping profile");
    assert_eq!(
        map.get(serde_yaml::Value::String("model".to_string()))
            .and_then(|v| v.as_str()),
        Some("anthropic/claude-sonnet-4")
    );
    assert_eq!(
        map.get(serde_yaml::Value::String("personality".to_string()))
            .and_then(|v| v.as_str()),
        Some("focused")
    );
    assert_eq!(
        map.get(serde_yaml::Value::String("max_turns".to_string()))
            .and_then(|v| v.as_i64()),
        Some(77)
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

#[test]
fn profile_alias_label_prefers_custom_aliases() {
    let mut aliases = std::collections::BTreeMap::new();
    aliases.insert("steve".to_string(), "steve".to_string());
    aliases.insert("qiaobusi".to_string(), "steve".to_string());
    aliases.insert("jobs".to_string(), "steve".to_string());
    aliases.insert("other".to_string(), "research".to_string());

    assert_eq!(
        profile_alias_label(&aliases, "steve").as_deref(),
        Some("aliases: jobs, qiaobusi")
    );
    assert_eq!(
        profile_alias_label(&aliases, "research").as_deref(),
        Some("alias: other")
    );
    assert_eq!(profile_alias_label(&aliases, "missing"), None);
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

    let cipher =
        <Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key).expect("cipher init");
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
fn read_gateway_pid_supports_plain_and_json_records() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plain = tmp.path().join("plain.pid");
    std::fs::write(&plain, "12345\n").expect("write plain pid");
    assert_eq!(read_gateway_pid(&plain), Some(12345));

    let json = tmp.path().join("json.pid");
    std::fs::write(
        &json,
        serde_json::json!({
            "pid": 23456,
            "kind": "hermes-gateway",
            "argv": ["hermes-gateway"]
        })
        .to_string(),
    )
    .expect("write json pid");
    assert_eq!(read_gateway_pid(&json), Some(23456));

    let invalid = tmp.path().join("invalid.pid");
    std::fs::write(&invalid, "{bad").expect("write invalid pid");
    assert_eq!(read_gateway_pid(&invalid), None);
}

#[test]
fn read_interactive_lock_pid_supports_plain_and_json_records() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plain = tmp.path().join("interactive.lock");
    std::fs::write(&plain, "12345\n").expect("write plain lock");
    assert_eq!(read_interactive_lock_pid(&plain), Some(12345));

    let json = tmp.path().join("interactive.json");
    std::fs::write(&json, r#"{"pid":23456}"#).expect("write json lock");
    assert_eq!(read_interactive_lock_pid(&json), Some(23456));
}

#[test]
fn query_is_local_slash_command_detects_prefixed_queries() {
    assert!(query_is_local_slash_command("/model list"));
    assert!(query_is_local_slash_command("   /graph status"));
    assert!(!query_is_local_slash_command("hello world"));
}

#[test]
fn interactive_tty_error_is_actionable() {
    let msg = interactive_tty_error_message();
    assert!(msg.contains("requires a terminal"));
    assert!(msg.contains("hermes-ultra setup"));
    assert!(msg.contains("chat --query"));
    assert!(msg.contains("doctor --deep --snapshot --bundle"));
}

#[test]
fn interactive_session_lock_guard_replaces_stale_pid_and_cleans_up() {
    let old_bypass = std::env::var_os(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
    std::env::remove_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let lock_path = interactive_lock_path_for_cli(&cli);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir lock parent");
    }
    std::fs::write(&lock_path, "999999").expect("write stale lock");
    let guard = InteractiveSessionLockGuard::acquire(&cli)
        .expect("acquire lock")
        .expect("guard enabled");
    assert_eq!(
        read_interactive_lock_pid(&lock_path),
        Some(std::process::id())
    );
    drop(guard);
    assert!(!lock_path.exists(), "lock file should be removed on drop");
    if let Some(value) = old_bypass {
        std::env::set_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV, value);
    }
}

#[cfg(unix)]
#[test]
fn interactive_session_lock_guard_rejects_live_pid() {
    let old_bypass = std::env::var_os(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
    std::env::remove_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let lock_path = interactive_lock_path_for_cli(&cli);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir lock parent");
    }
    // PID 1 should always be alive on Unix systems.
    std::fs::write(&lock_path, "1").expect("write lock");
    let err = match InteractiveSessionLockGuard::acquire(&cli) {
        Err(err) => err,
        Ok(_) => panic!("must reject live lock holder"),
    };
    let msg = format!("{err}");
    assert!(msg.contains("Another Hermes interactive session is running"));
    assert_eq!(read_interactive_lock_pid(&lock_path), Some(1));
    if let Some(value) = old_bypass {
        std::env::set_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV, value);
    }
}

#[cfg(unix)]
#[test]
fn parse_pid_snapshot_line_parses_ppid_tty_and_command() {
    let snap = parse_pid_snapshot_line("1 ?? /Users/sheawinkler/.cargo/bin/hermes-agent-ultra")
        .expect("snapshot");
    assert_eq!(snap.ppid, 1);
    assert_eq!(snap.tty, "??");
    assert!(snap.command.contains("hermes-agent-ultra"));
}

#[cfg(unix)]
#[test]
fn looks_like_interactive_hermes_process_matches_cli_and_not_gateway() {
    assert!(looks_like_interactive_hermes_process(
        "/Users/sheawinkler/.cargo/bin/hermes-agent-ultra"
    ));
    assert!(looks_like_interactive_hermes_process("hermes-ultra"));
    assert!(!looks_like_interactive_hermes_process(
        "/Users/sheawinkler/.cargo/bin/hermes-gateway"
    ));
}

#[test]
fn looks_like_gateway_process_includes_gateway_script_pattern() {
    assert!(looks_like_gateway_process(
        "python -m hermes_cli.main gateway run"
    ));
    assert!(looks_like_gateway_process(
        "python hermes_cli/main.py gateway run"
    ));
    assert!(looks_like_gateway_process("hermes gateway run"));
    assert!(looks_like_gateway_process(
        "hermes-gateway --config ~/.hermes"
    ));
    assert!(looks_like_gateway_process("python gateway/run.py"));
    assert!(!looks_like_gateway_process("python worker.py"));
}

#[test]
fn cleanup_stale_gateway_metadata_removes_pid_and_lock_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    std::fs::write(&pid_path, "999999\n").expect("write pid");
    std::fs::write(&lock_path, "{\"pid\":999999}").expect("write lock");

    cleanup_stale_gateway_metadata(&pid_path);
    assert!(!pid_path.exists());
    assert!(!lock_path.exists());
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
    telegram
        .extra
        .insert("webhook_secret".to_string(), serde_json::json!("tg-secret"));
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
    assert!(actions
        .iter()
        .any(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("created")));
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
    let pid_path = gateway_pid_path_for_cli(&cli);
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
fn resolve_resume_session_file_prefers_latest_modified_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let old = sessions_dir.join("old-session.json");
    let new = sessions_dir.join("new-session.json");
    std::fs::write(&old, r#"{"messages":[{"role":"user","content":"old"}]}"#)
        .expect("write old session");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&new, r#"{"messages":[{"role":"user","content":"new"}]}"#)
        .expect("write new session");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(resolved, "new-session");
    assert_eq!(path, new);
}

#[test]
fn resolve_resume_session_file_latest_prefers_canonical_session_stem() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let canonical = sessions_dir.join("c0ffee00-0000-4000-8000-000000000001.json");
    std::fs::write(
        &canonical,
        r#"{
  "session_info": {"session_id":"c0ffee00-0000-4000-8000-000000000001","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write canonical");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let named = sessions_dir.join("newest.json");
    std::fs::write(
        &named,
        r#"{
  "session_info": {"session_id":"snap-prune","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"snapshot payload"}]
}"#,
    )
    .expect("write named artifact");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(resolved, "c0ffee00-0000-4000-8000-000000000001");
    assert_eq!(path, canonical);
}

#[test]
fn resolve_resume_session_file_searches_session_id_when_exact_file_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let snapshot = sessions_dir.join("saved-snapshot-name.json");
    std::fs::write(
        &snapshot,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"}]
}"#,
    )
    .expect("write snapshot");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("20260603")).expect("resolve prefix");
    assert_eq!(resolved, "saved-snapshot-name");
    assert_eq!(path, snapshot);

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("ABCD12")).expect("resolve substring");
    assert_eq!(resolved, "saved-snapshot-name");
    assert_eq!(path, snapshot);
}

#[test]
fn resolve_resume_session_file_search_ranks_exact_before_prefix() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let exact = sessions_dir.join("snap-exact.json");
    std::fs::write(
        &exact,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"exact"}]
}"#,
    )
    .expect("write exact");
    let prefix = sessions_dir.join("20260603_090200_abcd12_child.json");
    std::fs::write(
        &prefix,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12_child","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"prefix"}]
}"#,
    )
    .expect("write prefix");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("20260603_090200_abcd12"))
            .expect("resolve exact session_info id");
    assert_eq!(resolved, "snap-exact");
    assert_eq!(path, exact);
}

#[test]
fn should_resume_fallback_to_fresh_only_for_latest_missing_state() {
    let latest_missing = AgentError::Config("No saved sessions found in /tmp".to_string());
    assert!(should_resume_fallback_to_fresh(None, &latest_missing));
    assert!(should_resume_fallback_to_fresh(
        Some("latest"),
        &latest_missing
    ));
    assert!(!should_resume_fallback_to_fresh(
        Some("abc123"),
        &latest_missing
    ));

    let other_error = AgentError::Config("Session 'abc123' not found".to_string());
    assert!(!should_resume_fallback_to_fresh(None, &other_error));
}

#[test]
fn load_resume_payload_restores_metadata_and_messages() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let session_path = sessions_dir.join("abc123.json");
    std::fs::write(
        &session_path,
        r#"{
  "session_info": {
"session_id": "session-xyz",
"model": "nous:openai/gpt-5.5-pro",
"personality": "technical"
  },
  "messages": [
{"role":"System","content":"[SESSION_OBJECTIVE] Keep context fresh"},
{"role":"User","content":"hello"},
{"role":"Assistant","content":"world"}
  ]
}"#,
    )
    .expect("write session");

    let payload = load_resume_payload(&cli, Some("abc123")).expect("load payload");
    assert_eq!(payload.resolved_id, "abc123");
    assert_eq!(payload.session_id, "session-xyz");
    assert_eq!(payload.model.as_deref(), Some("nous:openai/gpt-5.5-pro"));
    assert_eq!(payload.personality.as_deref(), Some("technical"));
    assert_eq!(payload.messages.len(), 3);
    assert!(matches!(
        payload.messages[0].role,
        hermes_core::MessageRole::System
    ));
    assert!(matches!(
        payload.messages[1].role,
        hermes_core::MessageRole::User
    ));
    assert!(matches!(
        payload.messages[2].role,
        hermes_core::MessageRole::Assistant
    ));
}

#[test]
fn load_resume_payload_follows_compression_tip_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let state_root = hermes_state_root(&cli);
    let sessions_dir = state_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("root.json"),
        r#"{
  "session_info": {"session_id":"root","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"pre-compression turn"}]
}"#,
    )
    .expect("write root session");
    std::fs::write(
        sessions_dir.join("cont.json"),
        r#"{
  "session_info": {"session_id":"cont","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"Assistant","content":"post-compression reply"}]
}"#,
    )
    .expect("write continuation session");

    let persistence = SessionPersistence::new(&state_root);
    persistence
        .persist_session(
            "root",
            &[hermes_core::Message::user("pre-compression turn")],
            Some("nous:openai/gpt-5.5"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    persistence
        .persist_session(
            "cont",
            &[hermes_core::Message::assistant("post-compression reply")],
            Some("nous:openai/gpt-5.5"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    let base = chrono::Utc::now() - chrono::Duration::hours(1);
    let root_created = base.to_rfc3339();
    let root_ended = (base + chrono::Duration::seconds(10)).to_rfc3339();
    let cont_created = (base + chrono::Duration::seconds(20)).to_rfc3339();
    assert!(persistence
        .update_session_lineage(
            "root",
            None,
            Some("compression"),
            Some(&root_created),
            Some(&root_ended),
        )
        .expect("mark root compressed"));
    assert!(persistence
        .update_session_lineage("cont", Some("root"), None, Some(&cont_created), None,)
        .expect("link continuation"));

    let payload = load_resume_payload(&cli, Some("root")).expect("load payload");

    assert_eq!(payload.resolved_id, "cont");
    assert_eq!(payload.session_id, "cont");
    assert_eq!(payload.source_path, sessions_dir.join("cont.json"));
    assert_eq!(payload.messages.len(), 1);
    assert_eq!(
        payload.messages[0].content.as_deref(),
        Some("post-compression reply")
    );
}

#[test]
fn load_resume_payload_falls_back_to_legacy_sessions_dir() {
    let _guard = env_lock();
    let prev_home = std::env::var("HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path().join("fake-home");
    let legacy_sessions = fake_home.join(".hermes").join("sessions");
    std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");
    let legacy_path = legacy_sessions.join("legacy-abc.json");
    std::fs::write(
        &legacy_path,
        r#"{
  "session_info": {
"session_id": "legacy-session",
"model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": [
{"role":"User","content":"from-legacy"}
  ]
}"#,
    )
    .expect("write legacy session");

    std::env::set_var("HOME", &fake_home);
    let state_root = tmp.path().join("ultra-state");
    let cli = cli_for_temp_state_root(&state_root);
    let payload = load_resume_payload(&cli, Some("legacy-abc")).expect("load payload");
    assert_eq!(payload.resolved_id, "legacy-abc");
    assert_eq!(payload.session_id, "legacy-session");
    assert_eq!(payload.messages.len(), 1);
    assert!(payload.source_path.starts_with(&legacy_sessions));

    match prev_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn load_resume_payload_accepts_empty_messages_for_startup_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let session_path = sessions_dir.join("empty-messages.json");
    std::fs::write(
        &session_path,
        r#"{
  "session_info": {
"session_id": "empty-messages",
"model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": []
}"#,
    )
    .expect("write empty session");

    let payload = load_resume_payload(&cli, Some("empty-messages")).expect("load payload");
    assert_eq!(payload.resolved_id, "empty-messages");
    assert_eq!(payload.session_id, "empty-messages");
    assert_eq!(
        payload.model.as_deref(),
        Some("nous:nousresearch/hermes-4-70b")
    );
    assert_eq!(payload.messages.len(), 0);
}

#[test]
fn load_resume_payload_latest_prefers_nonempty_snapshot_over_newer_empty_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let non_empty = sessions_dir.join("history-real.json");
    std::fs::write(
        &non_empty,
        r#"{
  "session_info": {"session_id":"history-real","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"},{"role":"Assistant","content":"world"}]
}"#,
    )
    .expect("write non-empty session");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let empty_snapshot = sessions_dir.join("startup-empty.json");
    std::fs::write(
        &empty_snapshot,
        r#"{
  "session_info": {"session_id":"startup-empty","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write empty session");

    let payload = load_resume_payload(&cli, None).expect("load payload");
    assert_eq!(payload.resolved_id, "history-real");
    assert_eq!(payload.messages.len(), 2);
    assert_eq!(payload.source_path, non_empty);
}

#[test]
fn load_resume_payload_latest_falls_back_to_legacy_nonempty_when_primary_empty_only() {
    let _guard = env_lock();
    let prev_home = std::env::var("HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path().join("fake-home");
    let legacy_sessions = fake_home.join(".hermes").join("sessions");
    std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");

    let legacy_non_empty = legacy_sessions.join("legacy-rich.json");
    std::fs::write(
        &legacy_non_empty,
        r#"{
  "session_info": {"session_id":"legacy-rich","model":"nous:nousresearch/hermes-4-70b"},
  "messages":[{"role":"User","content":"from legacy"}]
}"#,
    )
    .expect("write legacy non-empty session");

    std::env::set_var("HOME", &fake_home);
    let state_root = tmp.path().join("ultra-state");
    let cli = cli_for_temp_state_root(&state_root);
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("empty-only.json"),
        r#"{
  "session_info": {"session_id":"empty-only","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write primary empty session");

    let payload = load_resume_payload(&cli, None).expect("load payload");
    assert_eq!(payload.resolved_id, "legacy-rich");
    assert_eq!(payload.messages.len(), 1);
    assert!(payload.source_path.starts_with(&legacy_sessions));

    match prev_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[tokio::test]
async fn run_dump_writes_real_saved_session_export_with_system_prompt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("abc123.json"),
        r#"{
  "session_info": {
"session_id": "session-xyz",
"model": "nous:openai/gpt-5.5",
"personality": "technical",
"created_at": "2026-06-05T09:00:00Z"
  },
  "system_prompt": "persisted system prompt",
  "messages": [
{"role":"User","content":"hello"},
{"role":"Assistant","content":"world"}
  ]
}"#,
    )
    .expect("write session");

    run_dump(cli, Some("abc123".to_string()), None)
        .await
        .expect("dump session");

    let saved_dir = tmp.path().join("sessions").join("saved");
    let entries = std::fs::read_dir(&saved_dir)
        .expect("saved dir")
        .collect::<Result<Vec<_>, _>>()
        .expect("saved entries");
    assert_eq!(entries.len(), 1);
    let path = entries[0].path();
    assert!(path
        .file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|name| name.starts_with("hermes_conversation_")));

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read dump"))
            .expect("parse dump");
    assert_eq!(doc["session_id"], "session-xyz");
    assert_eq!(doc["resolved_id"], "abc123");
    assert_eq!(doc["model"], "nous:openai/gpt-5.5");
    assert_eq!(doc["personality"], "technical");
    assert_eq!(doc["system_prompt"], "persisted system prompt");
    assert_eq!(doc["session_start"], "2026-06-05T09:00:00Z");
    assert_eq!(doc["messages"].as_array().map(Vec::len), Some(2));
    assert!(doc["source_path"]
        .as_str()
        .is_some_and(|p| p.ends_with("abc123.json")));
}

#[test]
fn route_health_tier_marks_failure_streak_critical() {
    let stats = RouteLearningStatsRecord {
        samples: 8,
        success_rate: 0.61,
        avg_latency_ms: 2200.0,
        consecutive_failures: 6,
        updated_at_unix_ms: 1_700_000_000_000,
    };
    let (tier, reasons, score) = route_health_tier(&stats, route_learning_score(&stats));
    assert_eq!(tier, "critical");
    assert!(reasons.iter().any(|r| r == "failure_streak_critical"));
    assert!(score >= 0.0);
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

#[test]
fn parse_simple_env_file_supports_export_lines() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_path = tmp.path().join("route-autotune.env");
    std::fs::write(
        &env_path,
        "# comment\nexport HERMES_SMART_ROUTING_LEARNING_ALPHA=0.240\nHERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS=0.110\n",
    )
    .expect("write env");
    let parsed = parse_simple_env_file(&env_path);
    assert_eq!(
        parsed
            .get("HERMES_SMART_ROUTING_LEARNING_ALPHA")
            .map(String::as_str),
        Some("0.240")
    );
    assert_eq!(
        parsed
            .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
            .map(String::as_str),
        Some("0.110")
    );
}

#[test]
fn apply_route_autotune_env_overrides_sets_missing_keys_only() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "status",
    ]);
    let env_path = route_autotune_env_path_for_cli(&cli);
    if let Some(parent) = env_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(
        &env_path,
        "HERMES_SMART_ROUTING_LEARNING_ALPHA=0.300\nHERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN=0.050\n",
    )
    .expect("write env");

    std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
    std::env::set_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN", "0.999");
    let applied = apply_route_autotune_env_overrides(&cli);
    assert!(applied
        .iter()
        .any(|k| k == "HERMES_SMART_ROUTING_LEARNING_ALPHA"));
    assert_eq!(
        std::env::var("HERMES_SMART_ROUTING_LEARNING_ALPHA").ok(),
        Some("0.300".to_string())
    );
    assert_eq!(
        std::env::var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN").ok(),
        Some("0.999".to_string()),
        "explicit env var should not be overridden"
    );
    std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
    std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN");
}

#[test]
fn build_route_autotune_plan_raises_bias_for_critical_health() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "status",
    ]);
    let entry = RouteHealthEntry {
        key: "openai:gpt-4o".to_string(),
        health_score: 0.2,
        tier: "critical".to_string(),
        reasons: vec!["failure_streak_critical".to_string()],
        stats: RouteLearningStatsRecord {
            samples: 9,
            success_rate: 0.4,
            avg_latency_ms: 5200.0,
            consecutive_failures: 7,
            updated_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        },
    };
    let summary = serde_json::json!({
        "entries": 1,
        "overall": "critical",
        "average_score": 0.2,
        "healthy": 0,
        "watch": 0,
        "degraded": 0,
        "critical": 1
    });
    let plan = build_route_autotune_plan(
        &cli,
        Path::new("/tmp/route-learning.json"),
        Path::new("/tmp/route-health.json"),
        &[entry],
        &summary,
    );
    let cheap_bias = plan
        .overrides
        .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let switch_margin = plan
        .overrides
        .get("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    assert!(cheap_bias >= 0.14);
    assert!(switch_margin >= 0.05);
    assert_eq!(plan.confidence, "low");
}