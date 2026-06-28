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
