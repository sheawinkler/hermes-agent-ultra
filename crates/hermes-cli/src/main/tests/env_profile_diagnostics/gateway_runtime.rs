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

fn default_cron_scheduler(
    data_dir: std::path::PathBuf,
    run_runtime: Arc<RunRuntime>,
) -> Arc<CronScheduler> {
    let execution_mode = hermes_core::ResolvedExecutionMode::default();
    let execution_containment = Arc::new(
        hermes_environments::ExecutionContainmentContext::manual(execution_mode)
            .expect("default mode must admit manual containment"),
    );
    let tool_registry = Arc::new(hermes_agent::agent_loop::ToolRegistry::new());
    tool_registry.set_execution_containment_context(Arc::clone(&execution_containment));
    Arc::new(
        cron_scheduler_for_data_dir(
            data_dir,
            tool_registry,
            run_runtime,
            execution_mode,
            execution_containment,
        )
        .expect("test cron scheduler"),
    )
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
    let snap =
        parse_pid_snapshot_line("1 ?? /opt/hermes/bin/hermes-agent-ultra").expect("snapshot");
    assert_eq!(snap.ppid, 1);
    assert_eq!(snap.tty, "??");
    assert!(snap.command.contains("hermes-agent-ultra"));
}

#[cfg(unix)]
#[test]
fn looks_like_interactive_hermes_process_matches_cli_and_not_gateway() {
    assert!(looks_like_interactive_hermes_process(
        "/opt/hermes/bin/hermes-agent-ultra"
    ));
    assert!(looks_like_interactive_hermes_process("hermes-ultra"));
    assert!(!looks_like_interactive_hermes_process(
        "/opt/hermes/bin/hermes-gateway"
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
    let state = tempfile::tempdir().unwrap();
    let discord_spool = state.path().join("discord-route-spool.json");
    let run_runtime = Arc::new(
        RunRuntime::open_for_ephemeral_testing(state.path().join("run-runtime"))
            .expect("ephemeral run runtime"),
    );
    let cron_scheduler = default_cron_scheduler(state.path().join("cron"), run_runtime);
    register_gateway_adapters(
        &config,
        gateway.clone(),
        &mut sidecar_tasks,
        &discord_spool,
        cron_scheduler,
        hermes_core::ResolvedExecutionMode::default(),
    )
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
    let state = tempfile::tempdir().unwrap();
    let discord_spool = state.path().join("discord-route-spool.json");
    let run_runtime = Arc::new(
        RunRuntime::open_for_ephemeral_testing(state.path().join("run-runtime"))
            .expect("ephemeral run runtime"),
    );
    let cron_scheduler = default_cron_scheduler(state.path().join("cron"), run_runtime);
    register_gateway_adapters(
        &config,
        gateway.clone(),
        &mut sidecar_tasks,
        &discord_spool,
        cron_scheduler,
        hermes_core::ResolvedExecutionMode::default(),
    )
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
    let state = tempfile::tempdir().unwrap();
    let discord_spool = state.path().join("discord-route-spool.json");
    let run_runtime = Arc::new(
        RunRuntime::open_for_ephemeral_testing(state.path().join("run-runtime"))
            .expect("ephemeral run runtime"),
    );
    let cron_scheduler = default_cron_scheduler(state.path().join("cron"), run_runtime);
    register_gateway_adapters(
        &config,
        gateway.clone(),
        &mut sidecar_tasks,
        &discord_spool,
        cron_scheduler,
        hermes_core::ResolvedExecutionMode::default(),
    )
    .await
    .expect("qqbot and wecom_callback should register");

    let names = gateway.adapter_names().await;
    assert!(names.contains(&"qqbot".to_string()));
    assert!(names.contains(&"wecom_callback".to_string()));
}

#[cfg(unix)]
struct GatewayProcessTestChild {
    child: std::process::Child,
}

#[cfg(unix)]
impl GatewayProcessTestChild {
    fn spawn(script: &str, argv0: &str) -> Self {
        Self {
            child: std::process::Command::new("sh")
                .args(["-c", script, argv0])
                .spawn()
                .expect("spawn gateway process test child"),
        }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }
}

#[cfg(unix)]
impl Drop for GatewayProcessTestChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
fn wait_for_gateway_pid_state(pid: u32, expected: GatewayPidState) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let actual = gateway_pid_state(pid);
        if actual == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PID {pid} never reached {expected:?}; last state was {actual:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn write_gateway_process_metadata(pid_path: &std::path::Path, pid: u32) {
    std::fs::write(pid_path, format!("{pid}\n")).expect("write gateway PID");
    std::fs::write(
        gateway_lock_path_for_pid_path(pid_path),
        format!(r#"{{"pid":{pid},"kind":"hermes-gateway"}}"#),
    )
    .expect("write gateway lock");
}

#[cfg(unix)]
#[tokio::test]
async fn gateway_stop_keeps_metadata_until_delayed_gateway_exit_then_cleans_up() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    let mut child = GatewayProcessTestChild::spawn(
        "trap 'sleep 0.4; exit 0' TERM; while :; do sleep 1; done",
        "hermes-gateway",
    );
    let pid = child.id();
    wait_for_gateway_pid_state(pid, GatewayPidState::Gateway);
    write_gateway_process_metadata(&pid_path, pid);

    let stop_pid_path = pid_path.clone();
    let stop = tokio::spawn(async move {
        stop_gateway_pid_with_timeout(&stop_pid_path, pid, std::time::Duration::from_secs(2)).await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !stop.is_finished(),
        "stop must wait for the gateway's graceful drain"
    );
    assert!(
        pid_path.exists(),
        "PID metadata must survive while draining"
    );
    assert!(
        lock_path.exists(),
        "lock metadata must survive while draining"
    );

    let outcome = stop.await.expect("stop task").expect("graceful stop");
    assert_eq!(outcome, GatewayStopOutcome::Stopped);
    assert!(
        !pid_path.exists(),
        "PID metadata should be removed only after exit"
    );
    assert!(
        !lock_path.exists(),
        "lock metadata should be removed only after exit"
    );
    assert!(
        child.child.wait().expect("wait child").success(),
        "graceful child should exit successfully"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gateway_stop_timeout_preserves_metadata_for_a_still_live_gateway() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    let child =
        GatewayProcessTestChild::spawn("trap '' TERM; while :; do sleep 1; done", "hermes-gateway");
    let pid = child.id();
    wait_for_gateway_pid_state(pid, GatewayPidState::Gateway);
    write_gateway_process_metadata(&pid_path, pid);

    let error =
        stop_gateway_pid_with_timeout(&pid_path, pid, std::time::Duration::from_millis(150))
            .await
            .expect_err("uncooperative gateway must time out");
    assert!(
        matches!(error, AgentError::Timeout(_)),
        "expected a truthful timeout, got {error:?}"
    );
    assert!(pid_path.exists(), "PID metadata must remain after timeout");
    assert!(
        lock_path.exists(),
        "lock metadata must remain after timeout"
    );
    assert_eq!(gateway_pid_state(pid), GatewayPidState::Gateway);
}

#[cfg(unix)]
#[tokio::test]
async fn gateway_start_refuses_to_overlap_a_still_draining_gateway() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    let child =
        GatewayProcessTestChild::spawn("trap '' TERM; while :; do sleep 1; done", "hermes-gateway");
    let pid = child.id();
    wait_for_gateway_pid_state(pid, GatewayPidState::Gateway);
    write_gateway_process_metadata(&pid_path, pid);

    let timeout =
        stop_gateway_pid_with_timeout(&pid_path, pid, std::time::Duration::from_millis(150))
            .await
            .expect_err("uncooperative gateway must still be draining");
    assert!(matches!(timeout, AgentError::Timeout(_)));

    let error = ensure_gateway_start_slot_available(&pid_path)
        .expect_err("a still-draining gateway must block a new start");
    assert!(format!("{error}").contains("still running or draining"));
    assert!(pid_path.exists(), "start must not overwrite PID metadata");
    assert!(lock_path.exists(), "start must not overwrite lock metadata");

    std::fs::remove_file(&pid_path).expect("remove PID record for lock-only guard");
    let lock_only_error = ensure_gateway_start_slot_available(&pid_path)
        .expect_err("a live lock holder must also block a new start");
    assert!(
        format!("{lock_only_error}").contains("still running or draining"),
        "lock-only guard should retain the live gateway record"
    );
    assert!(
        !pid_path.exists(),
        "lock-only guard must not create replacement PID metadata"
    );
    assert!(
        lock_path.exists(),
        "lock-only guard must retain the live lock"
    );
}

#[cfg(unix)]
#[test]
fn gateway_start_keeps_live_lock_when_pid_record_is_stale() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    let child =
        GatewayProcessTestChild::spawn("trap '' TERM; while :; do sleep 1; done", "hermes-gateway");
    let live_pid = child.id();
    wait_for_gateway_pid_state(live_pid, GatewayPidState::Gateway);
    std::fs::write(&pid_path, "999999\n").expect("write stale PID record");
    std::fs::write(
        &lock_path,
        format!(r#"{{"pid":{live_pid},"kind":"hermes-gateway"}}"#),
    )
    .expect("write live lock");

    let error = ensure_gateway_start_slot_available(&pid_path)
        .expect_err("a live lock owner must prevent a new gateway start");
    assert!(
        format!("{error}").contains("still running or draining"),
        "the live lock must remain authoritative: {error}"
    );
    assert!(
        !pid_path.exists(),
        "only the independently dead PID record may be cleaned"
    );
    assert!(
        lock_path.exists(),
        "a live lock must never be deleted while preventing overlap"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gateway_stop_does_not_remove_a_different_live_lock_when_pid_is_dead() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    let mut exited = GatewayProcessTestChild::spawn("exit 0", "hermes-gateway");
    let exited_pid = exited.id();
    exited.child.wait().expect("reap exited gateway fixture");

    let live =
        GatewayProcessTestChild::spawn("trap '' TERM; while :; do sleep 1; done", "hermes-gateway");
    let live_pid = live.id();
    wait_for_gateway_pid_state(live_pid, GatewayPidState::Gateway);
    std::fs::write(&pid_path, format!("{exited_pid}\n")).expect("write exited PID");
    std::fs::write(
        &lock_path,
        format!(r#"{{"pid":{live_pid},"kind":"hermes-gateway"}}"#),
    )
    .expect("write live lock");

    let outcome =
        stop_gateway_pid_with_timeout(&pid_path, exited_pid, std::time::Duration::from_millis(1))
            .await
            .expect("already-exited PID is safe to clean");
    assert_eq!(outcome, GatewayStopOutcome::AlreadyExited);
    assert!(!pid_path.exists(), "only the exited PID record is removed");
    assert_eq!(read_gateway_pid(&lock_path), Some(live_pid));
}

#[cfg(unix)]
#[tokio::test]
async fn gateway_stop_refuses_to_signal_a_live_non_gateway_pid() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    let child = GatewayProcessTestChild::spawn(
        "trap '' TERM; while :; do sleep 1; done",
        "unrelated-worker",
    );
    let pid = child.id();
    wait_for_gateway_pid_state(pid, GatewayPidState::LiveUnowned);
    write_gateway_process_metadata(&pid_path, pid);

    let error =
        stop_gateway_pid_with_timeout(&pid_path, pid, std::time::Duration::from_millis(150))
            .await
            .expect_err("live non-gateway PID must never receive SIGTERM");
    assert!(format!("{error}").contains("does not identify as a Hermes gateway"));
    assert!(
        pid_path.exists(),
        "PID metadata must remain for an unowned PID"
    );
    assert!(
        lock_path.exists(),
        "lock metadata must remain for an unowned PID"
    );
    assert_eq!(gateway_pid_state(pid), GatewayPidState::LiveUnowned);
}

#[test]
fn gateway_cleanup_does_not_delete_replaced_pid_or_lock_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pid_path = tmp.path().join("gateway.pid");
    let lock_path = gateway_lock_path_for_pid_path(&pid_path);
    std::fs::write(&pid_path, "222\n").expect("write replacement PID");
    std::fs::write(&lock_path, r#"{"pid":222,"kind":"hermes-gateway"}"#)
        .expect("write replacement lock");

    cleanup_gateway_metadata_for_pid(&pid_path, 111);

    assert_eq!(read_gateway_pid(&pid_path), Some(222));
    assert_eq!(read_gateway_pid(&lock_path), Some(222));
}

#[test]
fn gateway_shutdown_preserves_discord_drain_error_after_global_finalizers() {
    let error = gateway_shutdown_result(
        Some(hermes_core::errors::GatewayError::Platform(
            "durable intake confirmation failed".to_string(),
        )),
        Ok(()),
    )
    .expect_err("drain failure must remain visible after gateway finalizers");

    match error {
        AgentError::Gateway(message) => {
            assert!(message.contains("Discord ingress drain failed after gateway cleanup"));
            assert!(message.contains("durable intake confirmation failed"));
        }
        other => panic!("expected gateway error, got {other:?}"),
    }
}

#[test]
fn gateway_shutdown_propagates_adapter_stop_failure_without_clean_success() {
    let error = gateway_shutdown_result(
        None,
        Err(hermes_core::errors::GatewayError::Platform(
            "platform adapter shutdown failed: failing:auth".to_string(),
        )),
    )
    .expect_err("adapter stop failure must make CLI shutdown fail");
    match error {
        AgentError::Gateway(message) => {
            assert!(message.contains("failing:auth"));
            assert!(
                !message.contains("Gateway stopped"),
                "a failed adapter shutdown must not be presented as clean"
            );
        }
        other => panic!("expected gateway error, got {other:?}"),
    }
}

struct StartupCleanupProbeAdapter {
    start_slot: Arc<std::sync::atomic::AtomicBool>,
    running: std::sync::atomic::AtomicBool,
    stop_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl hermes_core::PlatformAdapter for StartupCleanupProbeAdapter {
    async fn start(&self) -> Result<(), hermes_core::errors::GatewayError> {
        if self
            .start_slot
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(hermes_core::errors::GatewayError::Platform(
                "later adapter failed".to_string(),
            ));
        }
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<(), hermes_core::errors::GatewayError> {
        self.stop_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<hermes_core::ParseMode>,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn platform_name(&self) -> &str {
        "startup-cleanup-probe"
    }
}

#[tokio::test]
async fn gateway_start_failure_cleanup_stops_previously_started_adapters() {
    let gateway = make_gateway();
    let start_slot = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let first = Arc::new(StartupCleanupProbeAdapter {
        start_slot: start_slot.clone(),
        running: std::sync::atomic::AtomicBool::new(false),
        stop_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let second = Arc::new(StartupCleanupProbeAdapter {
        start_slot,
        running: std::sync::atomic::AtomicBool::new(false),
        stop_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    gateway.register_adapter("first", first.clone()).await;
    gateway.register_adapter("second", second.clone()).await;

    let start_error = gateway
        .start_all()
        .await
        .expect_err("the second adapter startup must fail");
    let running_before_cleanup = hermes_core::PlatformAdapter::is_running(first.as_ref()) as usize
        + hermes_core::PlatformAdapter::is_running(second.as_ref()) as usize;
    assert_eq!(
        running_before_cleanup, 1,
        "one adapter must have started before the later startup failure"
    );

    let error =
        cleanup_gateway_after_start_failure(gateway.as_ref(), None, start_error.into()).await;

    assert!(
        !hermes_core::PlatformAdapter::is_running(first.as_ref())
            && !hermes_core::PlatformAdapter::is_running(second.as_ref()),
        "cleanup must stop every adapter after startup failure"
    );
    assert_eq!(
        first.stop_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "cleanup must stop the first adapter exactly once"
    );
    assert_eq!(
        second.stop_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "cleanup must stop the failed adapter exactly once"
    );
    assert!(
        format!("{error}").contains("later adapter failed"),
        "startup error must remain actionable after cleanup"
    );
}

#[cfg(feature = "gateway-discord")]
#[test]
fn discord_spool_scope_quarantines_legacy_records_after_bot_rotation() {
    let temporary = tempfile::tempdir().expect("temporary state root");
    let cli = cli_for_temp_state_root(temporary.path());
    let mut old_config = hermes_config::GatewayConfig::default();
    let mut old_discord = make_platform(true, Some("old-opaque-token"));
    old_discord.extra.insert(
        "application_id".to_string(),
        serde_json::json!("111111111111111111"),
    );
    old_config
        .platforms
        .insert("discord".to_string(), old_discord.clone());
    let old_scope = DiscordAdapter::state_scope_for_config(&build_discord_config(
        &old_discord,
        old_config.proxy.as_ref(),
        platform_token_or_extra(&old_discord).expect("old Discord token"),
    ));

    let mut new_config = hermes_config::GatewayConfig::default();
    let mut new_discord = make_platform(true, Some("new-opaque-token"));
    new_discord.extra.insert(
        "application_id".to_string(),
        serde_json::json!("222222222222222222"),
    );
    new_config
        .platforms
        .insert("discord".to_string(), new_discord.clone());
    let new_scope = DiscordAdapter::state_scope_for_config(&build_discord_config(
        &new_discord,
        new_config.proxy.as_ref(),
        platform_token_or_extra(&new_discord).expect("new Discord token"),
    ));
    assert_ne!(
        old_scope, new_scope,
        "rotated bots need distinct state scopes"
    );

    let legacy_path = hermes_state_root(&cli)
        .join("gateway")
        .join("discord-route-spool.json");
    let mut legacy = DiscordRouteSpool::default();
    let mut unresolved = hermes_gateway::platforms::discord::IncomingDiscordMessage {
        channel_id: "old-channel".to_string(),
        channel_type: Some(0),
        guild_id: Some("old-guild".to_string()),
        parent_channel_id: None,
        thread_id: None,
        is_dm: false,
        message_id: "old-unresolved".to_string(),
        user_id: Some("old-user".to_string()),
        username: Some("Old User".to_string()),
        content: "old unresolved content".to_string(),
        is_bot: false,
        member_role_ids: Vec::new(),
        message_type: 0,
        mention_user_ids: Vec::new(),
        reply_to_message_id: None,
        reply_to_text: None,
        reply_to_attachments: Vec::new(),
        attachments: Vec::new(),
    };
    legacy.record_journal(
        &hermes_gateway::platforms::discord::DiscordJournalEntry::new(
            unresolved.clone(),
            Some("old-bot".to_string()),
        ),
    );
    unresolved.message_id = "old-partial".to_string();
    legacy.record_journal(
        &hermes_gateway::platforms::discord::DiscordJournalEntry::new(
            unresolved,
            Some("old-bot".to_string()),
        ),
    );
    legacy.set_partial_delivery(
        "old-partial",
        hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: "old-channel".to_string(),
            thread_id: None,
            reply_to_message_id: None,
            reply_to_mode: "none".to_string(),
            non_conversational: false,
            next_chunk_index: 1,
            remaining_text: "old partial suffix".to_string(),
            accepted_message_ids: vec!["old-accepted".to_string()],
            accepted_chunks: 1,
            retryable: true,
            failure: "old rate limit".to_string(),
            retry_after: Some(std::time::Duration::from_secs(30)),
            repair_required: None,
        },
    );
    write_discord_route_spool(&legacy_path, &legacy).expect("write legacy spool");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&legacy_path, std::fs::Permissions::from_mode(0o644))
            .expect("simulate legacy world-readable spool");
    }

    let new_path =
        discord_route_spool_path_for_gateway(&cli, &new_config).expect("resolve new scope");
    assert!(
        new_path.ends_with(
            std::path::Path::new("discord")
                .join(&new_scope)
                .join("route-spool.json")
        ),
        "new spool must live beneath the rotated bot's stable scope"
    );
    assert!(
        !new_path.exists(),
        "new bot must not adopt legacy spool bytes"
    );
    assert!(
        !legacy_path.exists(),
        "ambiguous fixed legacy path must be removed from active discovery"
    );
    assert!(
        DiscordRouteSpool::load(&new_path)
            .expect("load new scoped spool")
            .entries
            .is_empty(),
        "new bot must not replay old unresolved or partial work"
    );
    let quarantine = legacy_path
        .parent()
        .expect("legacy gateway parent")
        .join("discord-route-spool-quarantine");
    let quarantined = std::fs::read_dir(&quarantine)
        .expect("legacy spool quarantine")
        .collect::<Result<Vec<_>, _>>()
        .expect("read quarantine");
    assert_eq!(
        quarantined.len(),
        1,
        "legacy spool must be quarantined once"
    );
    let retained = std::fs::read_to_string(quarantined[0].path()).expect("read quarantined spool");
    assert!(retained.contains("old-unresolved"));
    assert!(retained.contains("old-partial"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&quarantine)
                .expect("quarantine directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "quarantine directory must not expose prompt or attachment data"
        );
        assert_eq!(
            std::fs::metadata(quarantined[0].path())
                .expect("quarantined spool metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "renaming a legacy 0644 spool must not retain its broad permissions"
        );
    }
}

struct ThreadStatusTargetProbeAdapter {
    status_targets: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl hermes_core::PlatformAdapter for ThreadStatusTargetProbeAdapter {
    async fn start(&self) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<hermes_core::ParseMode>,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        Ok(())
    }

    async fn send_or_update_status_with_options(
        &self,
        chat_id: &str,
        _status_key: &str,
        _text: &str,
        _parse_mode: Option<hermes_core::ParseMode>,
        options: hermes_core::SendMessageOptions,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        let target = options.thread_id.unwrap_or_else(|| chat_id.to_string());
        self.status_targets
            .lock()
            .expect("status target lock")
            .push(target);
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "thread-status-target-probe"
    }
}

#[tokio::test]
async fn gateway_status_callback_carries_context_thread_for_every_progress_status() {
    let gateway = make_gateway();
    let status_targets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    gateway
        .register_adapter(
            "discord",
            std::sync::Arc::new(ThreadStatusTargetProbeAdapter {
                status_targets: status_targets.clone(),
            }),
        )
        .await;
    let context = GatewayRuntimeContext {
        platform: "discord".to_string(),
        chat_id: "parent-channel".to_string(),
        thread_id: Some("thread-channel".to_string()),
        ..Default::default()
    };
    let status_callback = gateway_status_callback_for_context(gateway, &context);

    status_callback("tool:start", "Starting tool");
    status_callback("tool:complete", "Tool complete");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if status_targets.lock().expect("status target lock").len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both progress statuses must reach the adapter");
    let targets = status_targets.lock().expect("status target lock").clone();
    assert_eq!(
        targets,
        vec!["thread-channel".to_string(), "thread-channel".to_string(),],
        "every progress status must resolve to the Discord thread target"
    );
    assert!(
        !targets.iter().any(|target| target == "parent-channel"),
        "no progress status may resolve to the parent Discord channel"
    );
}
