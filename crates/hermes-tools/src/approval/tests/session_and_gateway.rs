#[test]
fn test_yolo_env_truthy_values_bypass_recoverable_confirmations() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let _session = EnvGuard::remove("HERMES_SESSION_KEY");
    let _sudo = EnvGuard::remove("SUDO_PASSWORD");
    let manager = ApprovalManager::new();

    for value in ["1", "true", "yes", "on"] {
        let _yolo = EnvGuard::set("HERMES_YOLO_MODE", value);
        assert_eq!(
            manager.check_approval_from_env("rm -rf /tmp/stuff", "local"),
            ApprovalDecision::Approved,
            "truthy HERMES_YOLO_MODE={value:?} should bypass recoverable approval"
        );
    }
}

#[test]
fn test_yolo_env_false_like_values_do_not_bypass() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let _session = EnvGuard::remove("HERMES_SESSION_KEY");
    let _sudo = EnvGuard::remove("SUDO_PASSWORD");
    let manager = ApprovalManager::new();

    for value in ["", "false", "False", "0", "off", "no"] {
        let _yolo = EnvGuard::set("HERMES_YOLO_MODE", value);
        assert_eq!(
            manager.check_approval_from_env("rm -rf /tmp/stuff", "local"),
            ApprovalDecision::RequiresConfirmation,
            "false-like HERMES_YOLO_MODE={value:?} must not bypass approval"
        );
    }
}

#[test]
fn test_cron_env_default_requires_confirmation_for_recoverable_commands() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let _session_key = EnvGuard::remove("HERMES_SESSION_KEY");
    let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
    let _sudo = EnvGuard::remove("SUDO_PASSWORD");
    let _cron_mode = EnvGuard::remove("HERMES_CRON_APPROVAL_MODE");
    let _cron_mode_legacy = EnvGuard::remove("HERMES_APPROVALS_CRON_MODE");
    let _cron = EnvGuard::set("HERMES_CRON_SESSION", "1");
    let manager = ApprovalManager::new();

    assert_eq!(
        manager.check_approval_from_env("rm -rf /tmp/stuff", "local"),
        ApprovalDecision::RequiresConfirmation
    );
}

#[test]
fn test_cron_env_approve_aliases_bypass_recoverable_only() {
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let _session_key = EnvGuard::remove("HERMES_SESSION_KEY");
    let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
    let _sudo = EnvGuard::remove("SUDO_PASSWORD");
    let _cron_mode_legacy = EnvGuard::remove("HERMES_APPROVALS_CRON_MODE");
    let _cron = EnvGuard::set("HERMES_CRON_SESSION", "1");
    let manager = ApprovalManager::new();

    for value in ["approve", "allow", "yes", "on", "true", "1", "off"] {
        let _cron_mode = EnvGuard::set("HERMES_CRON_APPROVAL_MODE", value);
        assert_eq!(
            manager.check_approval_from_env("rm -rf /tmp/stuff", "local"),
            ApprovalDecision::Approved,
            "cron approval mode {value:?} should allow recoverable commands"
        );
        assert_eq!(
            manager.check_approval_from_env("rm -rf /", "local"),
            ApprovalDecision::Denied,
            "cron approval mode {value:?} must not bypass hardline denial"
        );
    }
}

#[test]
fn test_cron_combined_guard_wins_over_gateway_origin() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "cron-gateway-origin";
    register_gateway_notify(session, |_request| {
        panic!("cron jobs must not submit gateway approval requests");
    });

    let result = check_all_command_guards_with_context(
        "rm -rf /tmp/cron-origin",
        "local",
        CommandGuardContext {
            cron_session: true,
            cron_approval_deny: true,
            gateway: true,
            ask: true,
            session_key: Some(session.to_string()),
            tirith_result: Ok(Some(TirithResult::allow())),
            ..CommandGuardContext::default()
        },
        None,
    )
    .expect("cron guard should return");

    assert!(!result.approved);
    assert!(result
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("cron jobs run without a user present"));
    assert!(!has_blocking_approval(session));
    unregister_gateway_notify(session);
    reset_approval_state_unlocked();
}

#[test]
fn test_session_scoped_yolo_only_bypasses_current_session() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
    let _sudo = EnvGuard::remove("SUDO_PASSWORD");
    let manager = ApprovalManager::new();

    clear_session("session-a");
    clear_session("session-b");
    enable_session_yolo("session-a");

    assert!(is_session_yolo_enabled("session-a"));
    assert!(!is_session_yolo_enabled("session-b"));

    {
        let _session = EnvGuard::set("HERMES_SESSION_KEY", "session-a");
        assert_eq!(
            manager.check_approval_from_env("rm -rf /tmp/stuff", "local"),
            ApprovalDecision::Approved,
            "session-a yolo should bypass recoverable approval"
        );
    }

    {
        let _session = EnvGuard::set("HERMES_SESSION_KEY", "session-b");
        assert_eq!(
            manager.check_approval_from_env("rm -rf /tmp/stuff", "local"),
            ApprovalDecision::RequiresConfirmation,
            "session-b must not inherit session-a yolo"
        );
    }

    clear_session("session-a");
    clear_session("session-b");
    reset_approval_state_unlocked();
}

#[test]
fn test_session_scoped_yolo_does_not_bypass_hardline_or_sudo_floor() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let _lock = TEST_ENV_LOCK.lock().unwrap();
    let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
    let _sudo = EnvGuard::remove("SUDO_PASSWORD");
    let _session = EnvGuard::set("HERMES_SESSION_KEY", "session-a");
    let manager = ApprovalManager::new();

    clear_session("session-a");
    enable_session_yolo("session-a");

    for command in ["rm -rf /", "mkfs.ext4 /dev/sda", "shutdown now"] {
        assert_eq!(
            manager.check_approval_from_env(command, "local"),
            ApprovalDecision::Denied,
            "session yolo must not bypass hardline denial for {command:?}"
        );
    }
    assert_eq!(
        manager.check_approval_from_env("sudo -S whoami", "local"),
        ApprovalDecision::Denied,
        "session yolo must not bypass sudo stdin/askpass denial"
    );

    clear_session("session-a");
    reset_approval_state_unlocked();
}

#[test]
fn test_clear_session_removes_session_yolo_state() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "clear-session-yolo";

    enable_session_yolo(session);
    assert!(is_session_yolo_enabled(session));

    clear_session(session);

    assert!(!is_session_yolo_enabled(session));
    reset_approval_state_unlocked();
}

#[test]
fn test_clear_session_removes_pattern_approval_state() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    approve_session("session-a", "recursive delete");
    approve_session("session-b", "recursive delete");

    assert!(is_approved("session-a", "recursive delete"));
    assert!(is_approved("session-b", "recursive delete"));

    clear_session("session-a");

    assert!(!is_approved("session-a", "recursive delete"));
    assert!(is_approved("session-b", "recursive delete"));
    reset_approval_state_unlocked();
}

#[test]
fn test_gateway_approval_resolve_single_is_fifo() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "gateway-fifo";
    let first = Arc::new(GatewayApprovalEntry::new(GatewayApprovalRequest {
        session_key: session.to_string(),
        command: "cmd1".to_string(),
        description: "first".to_string(),
        pattern_key: "first".to_string(),
        pattern_keys: vec!["first".to_string()],
        allow_permanent: true,
    }));
    let second = Arc::new(GatewayApprovalEntry::new(GatewayApprovalRequest {
        session_key: session.to_string(),
        command: "cmd2".to_string(),
        description: "second".to_string(),
        pattern_key: "second".to_string(),
        pattern_keys: vec!["second".to_string()],
        allow_permanent: true,
    }));
    GATEWAY_QUEUES
        .lock()
        .expect("gateway queue lock poisoned")
        .insert(
            session.to_string(),
            VecDeque::from([first.clone(), second.clone()]),
        );

    let count = resolve_gateway_approval(session, ApprovalChoice::Once, false);

    assert_eq!(count, 1);
    assert_eq!(first.result(), Some(ApprovalChoice::Once));
    assert_eq!(second.result(), None);
    assert_eq!(pending_gateway_approval_count(session), 1);
    reset_approval_state_unlocked();
}

#[test]
fn test_gateway_approval_resolve_all_unblocks_entries() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "gateway-all";
    let first = Arc::new(GatewayApprovalEntry::new(GatewayApprovalRequest {
        session_key: session.to_string(),
        command: "cmd1".to_string(),
        description: "first".to_string(),
        pattern_key: "first".to_string(),
        pattern_keys: vec!["first".to_string()],
        allow_permanent: true,
    }));
    let second = Arc::new(GatewayApprovalEntry::new(GatewayApprovalRequest {
        session_key: session.to_string(),
        command: "cmd2".to_string(),
        description: "second".to_string(),
        pattern_key: "second".to_string(),
        pattern_keys: vec!["second".to_string()],
        allow_permanent: true,
    }));
    GATEWAY_QUEUES
        .lock()
        .expect("gateway queue lock poisoned")
        .insert(
            session.to_string(),
            VecDeque::from([first.clone(), second.clone()]),
        );

    let count = resolve_gateway_approval(session, ApprovalChoice::Session, true);

    assert_eq!(count, 2);
    assert_eq!(first.result(), Some(ApprovalChoice::Session));
    assert_eq!(second.result(), Some(ApprovalChoice::Session));
    assert!(!has_blocking_approval(session));
    reset_approval_state_unlocked();
}

#[test]
fn test_clear_session_denies_and_signals_gateway_entries() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "gateway-boundary-cleanup";
    let first = Arc::new(GatewayApprovalEntry::new(GatewayApprovalRequest {
        session_key: session.to_string(),
        command: "cmd1".to_string(),
        description: "first".to_string(),
        pattern_key: "first".to_string(),
        pattern_keys: vec!["first".to_string()],
        allow_permanent: true,
    }));
    let second = Arc::new(GatewayApprovalEntry::new(GatewayApprovalRequest {
        session_key: session.to_string(),
        command: "cmd2".to_string(),
        description: "second".to_string(),
        pattern_key: "second".to_string(),
        pattern_keys: vec!["second".to_string()],
        allow_permanent: true,
    }));
    GATEWAY_QUEUES
        .lock()
        .expect("gateway queue lock poisoned")
        .insert(
            session.to_string(),
            VecDeque::from([first.clone(), second.clone()]),
        );

    clear_session(session);

    assert_eq!(first.result(), Some(ApprovalChoice::Deny));
    assert_eq!(second.result(), Some(ApprovalChoice::Deny));
    assert!(!has_blocking_approval(session));
    reset_approval_state_unlocked();
}

#[test]
fn test_combined_guards_gateway_blocks_until_resolved() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "gateway-e2e";
    let (tx, rx) = std::sync::mpsc::channel();
    register_gateway_notify(session, move |request| {
        tx.send(request).expect("notify request should send");
    });

    let session_for_thread = session.to_string();
    let handle = std::thread::spawn(move || {
        check_all_command_guards_with_context(
            "echo gateway-e2e",
            "local",
            CommandGuardContext {
                gateway: true,
                ask: true,
                session_key: Some(session_for_thread),
                gateway_approval_timeout: Duration::from_secs(5),
                tirith_result: Ok(Some(TirithResult::warn(
                    "gateway_unique_e2e",
                    "gateway warning",
                ))),
                ..CommandGuardContext::default()
            },
            None,
        )
        .expect("gateway guard should return")
    });

    let request = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("gateway notify should fire");
    assert_eq!(request.command, "echo gateway-e2e");
    assert_eq!(request.pattern_key, "tirith:gateway_unique_e2e");
    assert!(has_blocking_approval(session));

    assert_eq!(
        resolve_gateway_approval(session, ApprovalChoice::Session, false),
        1
    );
    let result = handle.join().expect("gateway guard thread should join");
    assert!(result.approved);
    assert!(result.user_approved);
    assert!(is_approved(session, "tirith:gateway_unique_e2e"));
    unregister_gateway_notify(session);
    reset_approval_state_unlocked();
}

#[test]
fn test_gateway_approval_notify_emits_redacted_command_but_queues_raw() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "gateway-redacted-egress";
    let fake_ghp = format!("ghp_{}", "X".repeat(36));
    let raw_command =
        format!("curl -H 'Authorization: token {fake_ghp}' https://api.github.com/user");
    let (tx, rx) = std::sync::mpsc::channel();
    register_gateway_notify(session, move |request| {
        tx.send(request).expect("notify request should send");
    });

    let session_for_thread = session.to_string();
    let command_for_thread = raw_command.clone();
    let handle = std::thread::spawn(move || {
        check_all_command_guards_with_context(
            &command_for_thread,
            "local",
            CommandGuardContext {
                gateway: true,
                ask: true,
                session_key: Some(session_for_thread),
                gateway_approval_timeout: Duration::from_secs(5),
                tirith_result: Ok(Some(TirithResult::warn(
                    "credential_redaction",
                    "credential-shaped command",
                ))),
                ..CommandGuardContext::default()
            },
            None,
        )
        .expect("gateway guard should return")
    });

    let display_request = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("gateway notify should fire");
    assert!(!display_request.command.contains(&fake_ghp));
    assert!(display_request.command.contains("github.com"));
    assert_eq!(display_request.pattern_key, "tirith:credential_redaction");

    let queued_raw = {
        let queues = GATEWAY_QUEUES.lock().expect("gateway queue lock poisoned");
        queues
            .get(session)
            .and_then(|entries| entries.front())
            .map(|entry| entry.request().command.clone())
            .expect("raw approval should remain queued")
    };
    assert_eq!(queued_raw, raw_command);

    assert_eq!(
        resolve_gateway_approval(session, ApprovalChoice::Session, false),
        1
    );
    let result = handle.join().expect("gateway guard thread should join");
    assert!(result.approved);
    assert!(result.user_approved);
    unregister_gateway_notify(session);
    reset_approval_state_unlocked();
}

#[test]
fn test_combined_guards_gateway_timeout_blocks() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "gateway-timeout";
    register_gateway_notify(session, |_request| {});

    let result = check_all_command_guards_with_context(
        "echo gateway-timeout",
        "local",
        CommandGuardContext {
            gateway: true,
            ask: true,
            session_key: Some(session.to_string()),
            gateway_approval_timeout: Duration::from_millis(10),
            tirith_result: Ok(Some(TirithResult::warn(
                "gateway_unique_timeout",
                "gateway warning",
            ))),
            ..CommandGuardContext::default()
        },
        None,
    )
    .expect("gateway guard should return");

    assert!(!result.approved);
    assert!(result
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("timed out"));
    assert!(!has_blocking_approval(session));
    unregister_gateway_notify(session);
    reset_approval_state_unlocked();
}

#[test]
fn test_combined_guards_gateway_without_listener_returns_pending() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let result = check_all_command_guards_with_context(
        "echo gateway-pending",
        "local",
        CommandGuardContext {
            ask: true,
            session_key: Some("gateway-no-listener".to_string()),
            tirith_result: Ok(Some(TirithResult::warn(
                "gateway_unique_pending",
                "gateway warning",
            ))),
            ..CommandGuardContext::default()
        },
        None,
    )
    .expect("gateway guard should return");

    assert!(!result.approved);
    assert_eq!(result.status.as_deref(), Some("pending_approval"));
    assert!(result.approval_pending);
    reset_approval_state_unlocked();
}

#[test]
fn test_approval_observers_fire_pre_and_post_on_cli_path() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_observer = events.clone();
    let observer_id = register_approval_observer(move |event| {
        events_for_observer.lock().unwrap().push(event);
    });
    let mut callback = |prompt: ApprovalPrompt| {
        assert_eq!(prompt.command, "rm -rf /tmp/observer-cli");
        ApprovalChoice::Once
    };

    let result = check_all_command_guards_with_context(
        "rm -rf /tmp/observer-cli",
        "local",
        CommandGuardContext {
            interactive: true,
            session_key: Some("observer-cli".to_string()),
            tirith_result: Ok(Some(TirithResult::allow())),
            ..CommandGuardContext::default()
        },
        Some(&mut callback),
    )
    .expect("approval guard should return");

    assert!(result.approved);
    assert!(result.user_approved);
    assert!(unregister_approval_observer(observer_id));
    let events = events.lock().unwrap().clone();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, ApprovalHookKind::PreApprovalRequest);
    assert_eq!(events[0].surface, ApprovalSurface::Cli);
    assert_eq!(events[0].session_key, "observer-cli");
    assert_eq!(events[0].choice, None);
    assert_eq!(events[1].kind, ApprovalHookKind::PostApprovalResponse);
    assert_eq!(events[1].surface, ApprovalSurface::Cli);
    assert_eq!(events[1].choice, Some(ApprovalChoice::Once));
    reset_approval_state_unlocked();
}

#[test]
fn test_approval_observer_panic_does_not_break_approval() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let observer_id = register_approval_observer(|_event| panic!("observer crashed"));
    let mut callback = |_prompt: ApprovalPrompt| ApprovalChoice::Once;

    let result = check_all_command_guards_with_context(
        "rm -rf /tmp/observer-panic",
        "local",
        CommandGuardContext {
            interactive: true,
            session_key: Some("observer-panic".to_string()),
            tirith_result: Ok(Some(TirithResult::allow())),
            ..CommandGuardContext::default()
        },
        Some(&mut callback),
    )
    .expect("approval guard should return despite observer panic");

    assert!(result.approved);
    assert!(unregister_approval_observer(observer_id));
    reset_approval_state_unlocked();
}

#[test]
fn test_approval_observers_fire_on_gateway_resolution() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let session = "observer-gateway";
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_observer = events.clone();
    let observer_id = register_approval_observer(move |event| {
        events_for_observer.lock().unwrap().push(event);
    });
    let (tx, rx) = std::sync::mpsc::channel();
    register_gateway_notify(session, move |request| {
        tx.send(request).expect("gateway request should send");
    });

    let handle = std::thread::spawn(move || {
        check_all_command_guards_with_context(
            "rm -rf /tmp/observer-gateway",
            "local",
            CommandGuardContext {
                gateway: true,
                ask: true,
                session_key: Some(session.to_string()),
                gateway_approval_timeout: Duration::from_secs(5),
                tirith_result: Ok(Some(TirithResult::allow())),
                ..CommandGuardContext::default()
            },
            None,
        )
        .expect("gateway guard should return")
    });

    let request = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("gateway notify should fire");
    assert_eq!(request.command, "rm -rf /tmp/observer-gateway");
    assert_eq!(
        resolve_gateway_approval(session, ApprovalChoice::Session, false),
        1
    );

    let result = handle.join().expect("gateway guard thread should join");
    assert!(result.approved);
    assert!(result.user_approved);
    assert!(unregister_approval_observer(observer_id));
    unregister_gateway_notify(session);
    let events = events.lock().unwrap().clone();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, ApprovalHookKind::PreApprovalRequest);
    assert_eq!(events[0].surface, ApprovalSurface::Gateway);
    assert_eq!(events[1].kind, ApprovalHookKind::PostApprovalResponse);
    assert_eq!(events[1].choice, Some(ApprovalChoice::Session));
    reset_approval_state_unlocked();
}

