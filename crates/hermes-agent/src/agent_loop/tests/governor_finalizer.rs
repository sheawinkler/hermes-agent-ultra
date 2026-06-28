#[test]
fn test_governor_reduces_budget_under_high_pressure() {
    let mut ctx = ContextManager::default_budget();
    let payload = "x".repeat(((ctx.max_context_chars() as f64) * 0.9) as usize);
    ctx.add_message(Message::user(payload));
    let config = AgentConfig {
        max_tokens: Some(1200),
        ..AgentConfig::default()
    };
    let gov = governor_for_turn(&config, &ctx, 12, None);
    assert!(gov.pressure >= 0.9);
    assert!(gov.max_tokens.unwrap_or(1200) < 1200);
    assert!(gov.tool_concurrency <= 4);
}

#[test]
fn test_governor_reduces_budget_under_latency_degradation() {
    let ctx = ContextManager::default_budget();
    let config = AgentConfig {
        max_tokens: Some(1200),
        ..AgentConfig::default()
    };
    let runtime = GovernorRuntimeState {
        avg_llm_latency_ms: Some(7000.0),
        avg_tool_error_rate: 0.0,
        consecutive_error_turns: 0,
    };
    let gov = governor_for_turn(&config, &ctx, 6, Some(&runtime));
    assert!(gov.latency_degraded);
    assert!(gov.max_tokens.unwrap_or(1200) < 1200);
    assert!(gov.tool_concurrency <= 2);
}

#[test]
fn test_governor_reduces_budget_under_error_degradation() {
    let ctx = ContextManager::default_budget();
    let config = AgentConfig {
        max_tokens: Some(1200),
        ..AgentConfig::default()
    };
    let runtime = GovernorRuntimeState {
        avg_llm_latency_ms: Some(1000.0),
        avg_tool_error_rate: 0.55,
        consecutive_error_turns: 3,
    };
    let gov = governor_for_turn(&config, &ctx, 10, Some(&runtime));
    assert!(gov.error_degraded);
    assert!(gov.max_tokens.unwrap_or(1200) < 1200);
    assert!(gov.tool_concurrency <= 2);
}

#[test]
fn test_tool_loop_guard_trips_on_consecutive_full_failure_turns() {
    assert!(!should_trip_tool_loop_guard_with_config(
        2, 2, 2, true, 3, 1
    ));
    assert!(should_trip_tool_loop_guard_with_config(3, 2, 2, true, 3, 1));
    assert!(!should_trip_tool_loop_guard_with_config(
        3, 2, 2, false, 3, 1
    ));
}

#[test]
fn test_tool_loop_guard_ignores_partial_success_turns() {
    assert!(!should_trip_tool_loop_guard_with_config(
        4, 3, 2, true, 2, 1
    ));
}

#[test]
fn test_looks_like_tool_error_output_detects_json_error_envelope() {
    assert!(looks_like_tool_error_output(
        r#"{"error":"Invalid tool parameters: Missing 'platform' parameter"}"#
    ));
    assert!(looks_like_tool_error_output(
        r#"{"success":false,"message":"failed"}"#
    ));
    assert!(!looks_like_tool_error_output(
        r#"{"success":true,"result":"ok"}"#
    ));
}

#[test]
fn test_looks_like_tool_error_output_detects_text_error_signatures() {
    assert!(looks_like_tool_error_output("error: invalid request"));
    assert!(looks_like_tool_error_output(
        "Invalid tool parameters: Missing 'platform' parameter"
    ));
    assert!(!looks_like_tool_error_output("all good"));
}

#[test]
fn test_redact_json_value_masks_sensitive_fields() {
    let mut payload = serde_json::json!({
        "api_key": "abc",
        "nested": { "token": "def", "safe": "ok" },
        "list": [{"password":"x"}, {"value":"y"}],
        "text": "Authorization: Bearer sk-secretvalue12345"
    });
    redact_json_value(&mut payload);
    assert_eq!(payload["api_key"], "[redacted]");
    assert_eq!(payload["nested"]["token"], "[redacted]");
    assert_eq!(payload["nested"]["safe"], "ok");
    assert_eq!(payload["list"][0]["password"], "[redacted]");
    assert_eq!(payload["text"], "Authorization: Bearer [redacted]");
}

#[test]
fn test_replay_recorder_adds_hash_chain_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let replay_path = tmp.path().join("trace.jsonl");
    let recorder = ReplayRecorder {
        path: Some(replay_path.clone()),
        state: Some(Arc::new(Mutex::new(ReplayState {
            seq: 0,
            prev_hash: short_sha256_hex("seed"),
            trace_root: short_sha256_hex("trace-seed"),
        }))),
    };

    recorder.record("turn_start", serde_json::json!({"token":"abc"}));
    recorder.record("tool_call", serde_json::json!({"cmd":"echo ok"}));

    let body = std::fs::read_to_string(&replay_path).expect("replay file");
    let mut lines = body.lines();
    let first: serde_json::Value =
        serde_json::from_str(lines.next().expect("line1")).expect("json line1");
    let second: serde_json::Value =
        serde_json::from_str(lines.next().expect("line2")).expect("json line2");

    assert_eq!(first["seq"], 1);
    assert_eq!(second["seq"], 2);
    assert!(first.get("trace_id").is_some());
    assert!(second.get("trace_id").is_some());
    assert_eq!(first["payload"]["token"], "[redacted]");
    assert_eq!(second["prev_hash"], first["event_hash"]);
    assert_ne!(first["event_hash"], second["event_hash"]);
}

#[test]
fn test_detect_contextlattice_connect_intent() {
    let msgs = vec![Message::user(
        "please confirm and connect to contextlattice, then harden it",
    )];
    assert!(detect_contextlattice_connect_intent(&msgs));

    let msgs = vec![Message::user("explain contextlattice architecture only")];
    assert!(!detect_contextlattice_connect_intent(&msgs));
}

#[test]
fn test_contextlattice_connect_system_hint_emitted() {
    let msgs = vec![Message::user("connect to contextlattice and verify health")];
    let hint = contextlattice_connect_system_hint(&msgs).expect("expected hint");
    assert!(hint.contains("contextlattice_search"));
    assert!(hint.contains("HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH"));
    assert!(hint.contains("Never use terminal command `contextlattice`"));
}

#[test]
fn test_contextlattice_intelligence_system_hint_requires_tools_and_intent() {
    let msgs = vec![Message::user(
        "perform deep repo audit and objective verification on /tmp/repo",
    )];
    let tools = vec![
        ToolSchema::new("contextlattice_search", "search", JsonSchema::new("object")),
        ToolSchema::new(
            "contextlattice_context_pack",
            "pack",
            JsonSchema::new("object"),
        ),
    ];
    let hint = contextlattice_intelligence_system_hint(&msgs, &tools).expect("expected hint");
    assert!(hint.contains("ContextLattice-first intelligence policy active"));
    assert!(hint.contains("scoped retrieval"));
    assert!(hint.contains("Copy numeric facts verbatim"));
}

#[test]
fn test_contextlattice_intelligence_system_hint_skips_without_tools() {
    let msgs = vec![Message::user(
        "perform deep repo audit and objective verification on /tmp/repo",
    )];
    let tools = vec![ToolSchema::new(
        "terminal",
        "terminal",
        JsonSchema::new("object"),
    )];
    assert!(contextlattice_intelligence_system_hint(&msgs, &tools).is_none());
}

#[test]
fn test_contextlattice_shell_invocation_detector() {
    assert!(is_contextlattice_shell_invocation(
        r#"{"command":"contextlattice"}"#
    ));
    assert!(is_contextlattice_shell_invocation(
        r#"{"command":"contextlattice status"}"#
    ));
    assert!(!is_contextlattice_shell_invocation(
        r#"{"command":"which contextlattice"}"#
    ));
    assert!(!is_contextlattice_shell_invocation(r#"{"command":"ls"}"#));
}

#[test]
fn test_repo_review_tool_profile_keeps_todo_filters_messaging() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "balanced");
    let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
    let mut calls = vec![
        ToolCall {
            id: "a".to_string(),
            function: hermes_core::FunctionCall {
                name: "todo".to_string(),
                arguments: "{}".to_string(),
            },
            extra_content: None,
        },
        ToolCall {
            id: "b".to_string(),
            function: hermes_core::FunctionCall {
                name: "telegram_send".to_string(),
                arguments: r#"{"text":"status"}"#.to_string(),
            },
            extra_content: None,
        },
    ];
    let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
    assert!(note.is_some());
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "todo");
}

#[test]
fn test_repo_review_tool_profile_escape_hatch_disables_filtering() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
    let msgs = vec![Message::user(
        "review repo at /tmp/app and diagnose issue; allow all tools",
    )];
    let mut calls = vec![ToolCall {
        id: "b".to_string(),
        function: hermes_core::FunctionCall {
            name: "telegram_send".to_string(),
            arguments: r#"{"text":"status"}"#.to_string(),
        },
        extra_content: None,
    }];
    let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
    assert!(note.is_some());
    assert_eq!(calls.len(), 1, "escape hatch should bypass filtering");
}

#[test]
fn test_repo_review_tool_profile_off_mode_disables_filtering() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "off");
    let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
    let mut calls = vec![ToolCall {
        id: "b".to_string(),
        function: hermes_core::FunctionCall {
            name: "telegram_send".to_string(),
            arguments: r#"{"text":"status"}"#.to_string(),
        },
        extra_content: None,
    }];
    let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
    assert!(note.is_none());
    assert_eq!(calls.len(), 1, "off mode should keep all calls");
}

#[test]
fn test_repo_review_discovery_policy_trims_repeated_loops() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "enforce");
    let msgs = vec![Message::user(
        "inspect repo /tmp/app and review codebase deeply",
    )];
    let mut state = RepoReviewBudgetState::default();
    let make_calls = || {
        vec![
            ToolCall {
                id: "1".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "2".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "3".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
        ]
    };
    let mut first = make_calls();
    assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
    let mut second = make_calls();
    assert!(
        apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state).is_none()
    );
    let mut third = make_calls();
    let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
    assert!(note.is_some());
    assert!(third.len() < 3);
}

#[test]
fn test_repo_review_discovery_policy_advisory_keeps_calls() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "advisory");
    let msgs = vec![Message::user(
        "inspect repo /tmp/app and review codebase deeply",
    )];
    let mut state = RepoReviewBudgetState::default();
    let mut first = vec![
        ToolCall {
            id: "1".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
            },
            extra_content: None,
        },
        ToolCall {
            id: "2".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
            },
            extra_content: None,
        },
        ToolCall {
            id: "3".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
            },
            extra_content: None,
        },
    ];
    assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
    let mut second = first.clone();
    let _ = apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state);
    let mut third = first.clone();
    let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
    assert!(note.is_some());
    assert_eq!(third.len(), 3, "advisory mode must not trim tool calls");
}

#[test]
fn test_repo_review_intent_includes_path_scoped_read_only_research() {
    let msgs = vec![Message::user(
        "Conduct READ-ONLY local research in /tmp/algotraderV2_rust and report back on how to improve profitability.",
    )];
    assert!(detect_repo_review_intent(&msgs));
    assert!(detect_research_evidence_intent(&msgs));
    let hint = exploratory_problem_solving_system_hint(&msgs).expect("research hint");
    assert!(hint.contains("Exploratory problem-solving protocol active"));
}

#[test]
fn test_finalizer_claim_retry_for_research_without_explicit_evidence() {
    let msgs = vec![Message::user(
        "Conduct read-only research in /tmp/algotraderV2_rust and report back with evidence-rich recommendations.",
    )];
    let answer = "Profitability can be improved by 60.2% based on local research.";
    assert!(finalizer_claim_requires_evidence_retry(&msgs, answer, 0));

    let grounded =
        "confidence=medium\nfile=Cargo.toml\ncmd=rg -n profit src\nObserved facts only.";
    assert!(!finalizer_claim_requires_evidence_retry(&msgs, grounded, 0));
}

#[test]
fn test_finalizer_claim_retry_for_missing_evidence_path() {
    let _lock = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).expect("create src");
    std::fs::write(tmp.path().join("src/lib.rs"), "fn main() {}\n").expect("write file");

    assert!(!assistant_references_missing_evidence_paths_from_base(
        "confidence=high\nfile=src/lib.rs:1\ncmd=rg -n main src/lib.rs",
        tmp.path()
    ));
    assert!(assistant_references_missing_evidence_paths_from_base(
        "confidence=high\nfile=src/missing.rs;cmd=rg -n main src/missing.rs",
        tmp.path()
    ));
}

#[test]
fn test_repo_research_plan_finalizer_requires_workstream_evidence() {
    let msgs = vec![Message::user(
        "Conduct read-only repo research in crates/hermes-agent/src/agent_loop.rs and report how to improve planning.",
    )];
    assert!(detect_exploratory_repo_research_intent(&msgs));

    let shallow = "confidence=medium\nfile=Cargo.toml\ncmd=rg -n planning crates";
    assert!(finalizer_repo_research_plan_requires_retry(
        &msgs, shallow, 0
    ));

    let grounded = "REPO_RESEARCH_PLAN: complete\n\
confidence=medium\n\
- workstream=web status=complete file=Cargo.toml cmd=rg -n web Cargo.toml\n\
- workstream=agent-loop status=complete file=src/agent_loop.rs cmd=rg -n finalizer src/agent_loop.rs\n\
- workstream=tests status=unproven file=Cargo.toml cmd=cargo test -p hermes-agent finalizer";
    assert!(!finalizer_repo_research_plan_requires_retry(
        &msgs, grounded, 0
    ));
}

#[test]
fn test_repo_research_plan_finalizer_accepts_explicit_blocker() {
    let msgs = vec![Message::user(
        "Research the missing repo path /tmp/does-not-exist and report blockers.",
    )];
    let blocked =
        "REPO_RESEARCH_PLAN: blocked\nblocker=path missing\ncmd=rg --files /tmp/does-not-exist";
    assert!(!finalizer_repo_research_plan_requires_retry(
        &msgs, blocked, 0
    ));
}

#[test]
fn test_tool_result_signal_score_rewards_workstream_evidence() {
    let score = tool_result_signal_score(
        "workstream=agent status=complete file=Cargo.toml path=crates/hermes-agent/src/agent_loop.rs cmd=rg -n finalizer crates command=cargo test",
        false,
    );
    assert!(score > 0.75, "score={score}");
}

#[test]
fn test_web_research_finalizer_requires_web_tool_and_urls() {
    let msgs = vec![Message::user(
        "Search the web for Solana trading strategies and cite concrete URLs.",
    )];
    assert!(detect_web_research_intent(&msgs));
    assert!(finalizer_web_research_requires_retry(
        &msgs,
        "WEB_SEARCH_USED: yes\nNo URLs found.",
        0
    ));

    let mut grounded = msgs.clone();
    grounded.push(Message::tool_result_with_name(
        "web1",
        "web_search",
        r#"{"results":[{"url":"https://docs.jito.wtf/lowlatencytxnsend/"}]}"#,
    ));
    let search_only_answer = "WEB_SEARCH_USED: yes\nSOURCE_QUALITY: primary=1 community=1 secondary=0\nObserved:\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
    assert!(finalizer_web_research_requires_retry(
        &grounded,
        search_only_answer,
        0
    ));
    grounded.push(Message::tool_result_with_name(
        "web2",
        "web_extract",
        "Extracted Jito low-latency transaction send docs.",
    ));
    let answer = "WEB_SEARCH_USED: yes\nSOURCE_QUALITY: primary=1 community=1 secondary=0\nObserved:\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
    assert!(!finalizer_web_research_requires_retry(&grounded, answer, 0));
}

#[test]
fn test_web_research_finalizer_requires_source_quality_counts() {
    let mut msgs = vec![Message::user(
        "Search the web for Solana trading strategies and cite concrete URLs.",
    )];
    msgs.push(Message::tool_result_with_name(
        "web1",
        "web_search",
        r#"{"results":[{"url":"https://docs.jito.wtf/lowlatencytxnsend/"}]}"#,
    ));
    msgs.push(Message::tool_result_with_name(
        "web2",
        "web_extract",
        "Extracted source",
    ));
    let missing_quality = "WEB_SEARCH_USED: yes\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
    assert!(finalizer_web_research_requires_retry(
        &msgs,
        missing_quality,
        0
    ));
}

#[test]
fn test_web_research_system_hint_reports_tool_availability() {
    let msgs = vec![Message::user(
        "Do online research across the web and cite URLs.",
    )];
    let tools = vec![ToolSchema::new(
        "web_search",
        "Search the web",
        JsonSchema::new("object"),
    )];
    let hint = web_research_system_hint(&msgs, &tools).expect("web hint");
    assert!(hint.contains("Web research contract active"));
    assert!(hint.contains("web_search"));
    assert!(hint.contains("SOURCE_QUALITY"));
}

#[test]
fn test_task_focus_finalizer_retries_when_explicit_anchors_disappear() {
    let msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    assert!(finalizer_task_focus_requires_retry(
        &msgs,
        "Here is a generic repository analysis with no email evidence.",
        0
    ));
    assert!(!finalizer_task_focus_requires_retry(
        &msgs,
        "Gmail is blocked: not authenticated for sheawinkler@gmail.com.",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_retries_absent_skill_claim() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::tool_result_with_name(
        "skill1",
        "skill_view",
        "# Google Workspace\nGmail, Calendar, Drive.",
    ));

    assert!(finalizer_google_workspace_requires_retry(
        &msgs,
        "No Google Workspace tools exist, so this is blocked.",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_requires_status_marker() {
    let msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    assert!(finalizer_google_workspace_requires_retry(
        &msgs,
        "Here is an unrelated repo analysis.",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_accepts_setup_probe_blocker() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::assistant_with_tool_calls(
        None,
        vec![hermes_core::ToolCall {
            id: "call_setup".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check"}"#.to_string(),
            },
            extra_content: None,
        }],
    ));
    msgs.push(Message::tool_result_with_name(
        "call_setup",
        "terminal",
        r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
    ));

    assert!(!finalizer_google_workspace_requires_retry(
        &msgs,
        "GOOGLE_WORKSPACE_USED: no\ncmd=python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check\nerror=NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_retries_success_after_auth_blocker() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::assistant_with_tool_calls(
        None,
        vec![hermes_core::ToolCall {
            id: "call_setup".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"env HERMES_HOME=/Users/me/.hermes-agent-ultra python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check"}"#.to_string(),
            },
            extra_content: None,
        }],
    ));
    msgs.push(Message::tool_result_with_name(
        "call_setup",
        "terminal",
        r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
    ));

    assert!(finalizer_google_workspace_requires_retry(
        &msgs,
        "GOOGLE_WORKSPACE_USED: yes\n20 important emails were found. Gmail search and reading were successful.",
        0
    ));
}

#[test]
fn test_google_workspace_auth_blocker_guard_blocks_setup_mutation() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::tool_result_with_name(
        "call_setup",
        "terminal",
        r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
    ));
    let calls = vec![hermes_core::ToolCall {
        id: "write_fake".to_string(),
        function: hermes_core::FunctionCall {
            name: "write_file".to_string(),
            arguments: r#"{"path":"/tmp/simulated_clients.json","content":"{}"}"#.to_string(),
        },
        extra_content: None,
    }];

    assert!(google_workspace_auth_blocker_mutation_guard(&msgs, &calls).is_some());
    let auth_url_calls = vec![hermes_core::ToolCall {
        id: "auth_url".to_string(),
        function: hermes_core::FunctionCall {
            name: "terminal".to_string(),
            arguments: r#"{"command":"env HERMES_HOME=/Users/me/.hermes-agent-ultra python3 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --auth-url --services email"}"#.to_string(),
        },
        extra_content: None,
    }];
    assert!(google_workspace_auth_blocker_mutation_guard(&msgs, &auth_url_calls).is_some());
}

#[test]
fn test_terminal_command_system_hint_warns_against_shell_wrappers() {
    let tools = vec![ToolSchema::new(
        "terminal",
        "Execute command",
        JsonSchema::new("object"),
    )];
    let hint = terminal_command_system_hint(&tools).expect("terminal hint");
    assert!(hint.contains("bash -lc"));
    assert!(hint.contains("direct commands"));
}

#[test]
fn test_finalizer_output_quality_retry_detects_placeholders() {
    let templated =
        "**Title:** Example\n**Authors:** pack of authors\n(Full text available at [URL](URL))";
    assert!(finalizer_output_quality_requires_retry(templated, 0));
}

#[test]
fn test_finalizer_output_quality_retry_detects_fake_attachments() {
    let answer = "The full evidence is attached separately; proposed calibration: redacted.";
    assert!(finalizer_output_quality_requires_retry(answer, 0));
    assert!(finalizer_output_quality_requires_retry(
        r#"{"name":"terminal","arguments":{}}</tool_call>"#,
        0
    ));
}

#[test]
fn test_finalizer_output_quality_retry_detects_duplicate_lines() {
    let duplicated =
        "- **Title:** Bayesian Learning for Dive State Prediction and Management\n\
        - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
        - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
        - **Title:** Bayesian Learning for Dive State Prediction and Management";
    assert!(finalizer_output_quality_requires_retry(duplicated, 0));
    assert!(!finalizer_output_quality_requires_retry(duplicated, 2));
}

#[test]
fn test_finalizer_action_execution_retry_detects_intent_narration() {
    let msgs = vec![Message::user(
        "proceed with deep repo review for /tmp/app and implement patches",
    )];
    assert!(finalizer_action_execution_requires_retry(
        &msgs,
        "I will proceed now and report back shortly.",
        0
    ));
    assert!(!finalizer_action_execution_requires_retry(
        &msgs,
        "I will proceed now and report back shortly.",
        2
    ));
}

#[test]
fn test_finalizer_action_execution_retry_skips_when_evidence_present() {
    let msgs = vec![Message::user(
        "proceed with deep repo review for /tmp/app and implement patches",
    )];
    assert!(!finalizer_action_execution_requires_retry(
        &msgs,
        "cmd=rg -n TODO src\nfile=/tmp/app/src/main.rs\nobjective_state=advancing",
        0
    ));
}

#[test]
fn test_objective_guard_requires_sections_for_trading_objective() {
    let msgs = vec![
        Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
        Message::user("review repo /tmp/algotraderv2_rust and produce patch plan"),
    ];
    let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
    assert!(active);
    assert!(needs_analytics);
    assert!(!deep_audit_required);
    assert!(!objective_guard_satisfied("plain response", true, false));
    assert!(objective_guard_satisfied(
        "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL",
        true,
        false
    ));
}

#[test]
fn test_deep_objective_guard_requires_deep_audit_section() {
    let msgs = vec![
        Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
        Message::user(
            "deep end-to-end review repo /tmp/algotraderv2_rust and produce complete patch plan",
        ),
    ];
    let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
    assert!(active);
    assert!(needs_analytics);
    assert!(deep_audit_required);

    let shallow = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL";
    assert!(!objective_guard_satisfied(shallow, true, true));

    let numeric_only = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- verified_files=8\n- commands_run=5\n- unknowns=1\n- blockers=none";
    assert!(!objective_guard_satisfied(numeric_only, true, true));

    let deep = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=complete evidence(file=/tmp/b.rs cmd=sed -n 1,220p src/strategy.rs)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test -p hermes-agent objective_guard)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=none";
    assert!(objective_guard_satisfied(deep, true, true));
}

#[test]
fn test_deep_objective_retry_prompt_contains_audit_requirements() {
    let prompt = objective_guard_retry_prompt(true, true);
    assert!(prompt.contains(OBJECTIVE_DEEP_AUDIT_TAG));
    assert!(prompt.contains("file=<verified_path_1>"));
    assert!(prompt.contains("cmd=<command_1>"));
    assert!(prompt.contains("workstream=<name> status=<complete|blocked|unproven>"));
}

#[test]
fn test_deep_objective_scope_complete_rejects_non_complete_streams() {
    let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=blocked evidence(file=/tmp/b.rs cmd=rg -n strategy src)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=rpc unavailable";
    assert!(!objective_guard_satisfied(text, true, true));
}

#[test]
fn test_coerce_textual_tool_calls_extracts_and_cleans_message() {
    let msg = Message::assistant(
        "Proceeding with discovery now.\n<tool_call name=\"skill_view\">\n<argument name=\"skill\">contextlattice-master-router</argument>\n</tool_call>",
    );
    let (coerced, calls, parsed_textual) = AgentLoop::coerce_textual_tool_calls(msg);
    assert!(parsed_textual);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "skill_view");
    assert_eq!(
        coerced.content.as_deref(),
        Some("Proceeding with discovery now.")
    );
}

#[test]
fn test_coerce_textual_tool_calls_keeps_declared_calls() {
    let msg = Message::assistant_with_tool_calls(
        Some("Running tool.".to_string()),
        vec![ToolCall {
            id: "id1".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            },
            extra_content: None,
        }],
    );
    let (coerced, calls, parsed_textual) = AgentLoop::coerce_textual_tool_calls(msg);
    assert!(!parsed_textual);
    assert_eq!(calls.len(), 1);
    assert_eq!(coerced.tool_calls.as_ref().map(|v| v.len()), Some(1));
    assert_eq!(coerced.content.as_deref(), Some("Running tool."));
}

#[test]
fn test_extract_objective_state_marker_prefers_explicit_marker() {
    let text = "ANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.12 SOL";
    assert_eq!(extract_objective_state_marker(text), "advancing");
    let colon_text = "ANALYTICS_VERIFIED:\n- objective_state: regressing metric=-0.30 SOL";
    assert_eq!(extract_objective_state_marker(colon_text), "regressing");
}

#[test]
fn test_extract_marker_values_collects_unique_paths_and_cmds() {
    let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nDEEP_AUDIT_VERIFIED:\n- cmd=rg -n objective src\n- cmd=cargo test -p hermes-agent objective_guard";
    let files = extract_marker_values(text, "path=", 8);
    let cmds = extract_marker_values(text, "cmd=", 8);
    assert_eq!(
        files,
        vec!["/tmp/a.rs".to_string(), "/tmp/b.rs".to_string()]
    );
    assert_eq!(cmds, vec!["rg".to_string(), "cargo".to_string()]);
}
