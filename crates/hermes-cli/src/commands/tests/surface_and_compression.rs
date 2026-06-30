    #[test]
    fn test_acp_setup_browser_dependency_checks_reports_browser_failure() {
        let mut calls = Vec::new();
        let err = acp_setup_browser_dependency_checks(false, |command| {
            calls.push(command.to_string());
            command == "node"
        })
        .expect_err("browser dependency failure should be reported");

        assert_eq!(calls, vec!["node", "agent-browser"]);
        assert!(err.to_string().contains("browser"));
    }

    #[test]
    fn test_start_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/start"));
        let results = autocomplete("/sta");
        assert!(results.contains(&"/start"));
    }

    #[test]
    fn test_pet_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/pet"));
        let results = autocomplete("/pe");
        assert!(results.contains(&"/pet"));
    }

    #[test]
    fn test_objective_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/objective"));
        let results = autocomplete("/obj");
        assert!(results.contains(&"/objective"));
    }

    #[tokio::test]
    async fn harness_command_reports_issue_backed_cockpit_and_teach_skill() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/harness"));
        assert!(autocomplete("/har").contains(&"/harness"));

        handle_slash_command(&mut app, "/harness", &["json"])
            .await
            .expect("harness json");
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("https://github.com/sheawinkler/hermes-agent-ultra/issues/702"));
        assert!(output.contains("\"teach\""));
        assert!(output.contains("\"Rust dashboard OIDC loader\""));

        handle_slash_command(&mut app, "/harness", &["skills"])
            .await
            .expect("harness skills");
        let skills_output = latest_ui_assistant_text(&app);
        assert!(skills_output.contains("\"mattpocock\""));
        assert!(skills_output.contains("\"teach\""));
    }

    #[test]
    fn test_handoff_and_subgoal_commands_are_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/handoff"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/subgoal"));
        let handoff_results = autocomplete("/han");
        assert!(handoff_results.contains(&"/handoff"));
        let subgoal_results = autocomplete("/sub");
        assert!(subgoal_results.contains(&"/subgoal"));
    }

    #[test]
    fn test_kanban_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/kanban"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/tasks"));
        let results = autocomplete("/kan");
        assert!(results.contains(&"/kanban"));
    }

    #[test]
    fn test_parse_kanban_add_defaults() {
        let input = parse_kanban_add(&["Ship", "kanban"]).expect("parse");
        assert_eq!(input.title, "Ship kanban");
        assert_eq!(input.lane, KanbanLane::Todo);
        assert_eq!(input.priority, 3);
        assert!(!input.goal_mode);
        assert_eq!(input.goal_max_turns, None);
    }

    #[test]
    fn test_parse_kanban_add_flags() {
        let input = parse_kanban_add(&[
            "Task",
            "--lane",
            "doing",
            "--priority",
            "2",
            "--assignee",
            "runner",
            "--depends",
            "K-0001,K-0002",
            "--desc",
            "note",
            "--goal",
            "--goal-max-turns",
            "7",
        ])
        .expect("parse");
        assert_eq!(input.title, "Task");
        assert_eq!(input.lane, KanbanLane::Doing);
        assert_eq!(input.priority, 2);
        assert_eq!(input.assignee.as_deref(), Some("runner"));
        assert_eq!(input.depends_on, vec!["K-0001", "K-0002"]);
        assert_eq!(input.description.as_deref(), Some("note"));
        assert!(input.goal_mode);
        assert_eq!(input.goal_max_turns, Some(7));
    }

    #[test]
    fn test_goal_alias_maps_to_objective() {
        assert_eq!(canonical_command("/goal"), "/objective");
    }

    #[test]
    fn test_golden_upstream_surface_aliases_are_registered() {
        for command in [
            "/bp",
            "/v",
            "/credits",
            "/billing",
            "/suggest",
            "/suggestions",
        ] {
            assert!(
                SLASH_COMMANDS.iter().any(|(name, _)| *name == command),
                "{command} should be registered"
            );
        }
        assert_eq!(canonical_command("/bp"), "/blueprint");
        assert_eq!(canonical_command("/v"), "/version");
        assert_eq!(canonical_command("/credits"), "/usage");
        assert_eq!(canonical_command("/billing"), "/billing");
        assert_eq!(canonical_command("/suggest"), "/suggestions");
        assert!(autocomplete("/cre").contains(&"/credits"));
        assert!(autocomplete("/bill").contains(&"/billing"));
        assert!(autocomplete("/sugg").contains(&"/suggestions"));
    }

    #[tokio::test]
    async fn suggestions_catalog_accept_and_dismiss_flow() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let empty = handle_slash_command(&mut app, "/suggestions", &[])
            .await
            .expect("empty suggestions");
        assert_eq!(empty, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("No suggested automations"));

        handle_slash_command(&mut app, "/suggestions", &["catalog"])
            .await
            .expect("seed catalog");
        let seeded = latest_ui_assistant_text(&app);
        assert!(seeded.contains("Added"));
        assert!(seeded.contains("Morning briefing"));

        handle_slash_command(&mut app, "/suggest", &[])
            .await
            .expect("alias list suggestions");
        let listed = latest_ui_assistant_text(&app);
        assert!(listed.contains("Suggested automations"));
        assert!(listed.contains("Important-mail monitor"));

        handle_slash_command(&mut app, "/suggestions", &["accept", "1"])
            .await
            .expect("accept suggestion");
        let accepted = latest_ui_assistant_text(&app);
        assert!(accepted.contains("Scheduled 'Morning briefing'"));
        let jobs = app.cron_scheduler.list_jobs().await;
        assert!(jobs.iter().any(|job| {
            job.name.as_deref() == Some("Morning briefing") && job.schedule == "0 8 * * *"
        }));

        handle_slash_command(&mut app, "/suggestions", &["dismiss", "1"])
            .await
            .expect("dismiss suggestion");
        assert!(latest_ui_assistant_text(&app).contains("Dismissed."));

        handle_slash_command(&mut app, "/suggestions", &["clear"])
            .await
            .expect("clear suggestions");
        assert!(latest_ui_assistant_text(&app).contains("Cleared 1 resolved"));
    }

    #[tokio::test]
    async fn objective_lifecycle_pause_resume_updates_session_injection() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let set_result = handle_slash_command(&mut app, "/objective", &["stabilize", "indexing"])
            .await
            .expect("set objective");
        assert_eq!(set_result, CommandResult::Handled);
        assert_eq!(app.session_objective.as_deref(), Some("stabilize indexing"));

        let pause_result =
            handle_slash_command(&mut app, "/objective", &["pause", "manual", "hold"])
                .await
                .expect("pause objective");
        assert_eq!(pause_result, CommandResult::Handled);
        assert!(app.session_objective.is_none());
        assert!(latest_ui_assistant_text(&app).contains("status=paused"));

        let resume_result = handle_slash_command(&mut app, "/objective", &["resume", "continue"])
            .await
            .expect("resume objective");
        assert_eq!(resume_result, CommandResult::Handled);
        assert_eq!(app.session_objective.as_deref(), Some("stabilize indexing"));
        assert!(latest_ui_assistant_text(&app).contains("status=active"));
    }

    #[tokio::test]
    async fn objective_wait_pid_and_unwait_update_contract() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/objective", &["stabilize", "release"])
            .await
            .expect("set objective");

        let wait_result = handle_slash_command(
            &mut app,
            "/objective",
            &["wait", "4242", "CI", "still", "running"],
        )
        .await
        .expect("set wait pid");
        assert_eq!(wait_result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Objective wait barrier set"));
        assert!(output.contains("pid=4242"));
        let contract = load_objective_contract()
            .expect("load contract")
            .expect("contract");
        assert_eq!(contract.waiting_on_pid, Some(4242));
        assert_eq!(contract.waiting_reason, "CI still running");

        let unwait_result = handle_slash_command(&mut app, "/goal", &["unwait"])
            .await
            .expect("unwait");
        assert_eq!(unwait_result, CommandResult::Handled);
        let contract = load_objective_contract()
            .expect("load contract")
            .expect("contract");
        assert!(contract.waiting_on_pid.is_none());
        assert!(contract.waiting_on_session.is_none());
        assert_eq!(contract.waiting_until_unix_ms, 0);
    }

    #[tokio::test]
    async fn objective_wait_supports_session_and_seconds_forms() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/objective", &["ship", "build"])
            .await
            .expect("set objective");

        handle_slash_command(
            &mut app,
            "/objective",
            &["wait", "--session", "proc_abc", "build", "watcher"],
        )
        .await
        .expect("set session wait");
        let contract = load_objective_contract()
            .expect("load contract")
            .expect("contract");
        assert_eq!(contract.waiting_on_session.as_deref(), Some("proc_abc"));
        assert_eq!(contract.waiting_reason, "build watcher");

        handle_slash_command(
            &mut app,
            "/objective",
            &["wait", "for", "30s", "rate", "limit"],
        )
        .await
        .expect("set seconds wait");
        let contract = load_objective_contract()
            .expect("load contract")
            .expect("contract");
        assert!(contract.waiting_on_session.is_none());
        assert!(contract.waiting_until_unix_ms > crate::alpha_runtime::objective_now_unix_ms());
        assert_eq!(contract.waiting_reason, "rate limit");
        assert!(latest_ui_assistant_text(&app).contains("remaining_seconds="));
    }

    #[tokio::test]
    async fn objective_behavior_mode_can_be_switched() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/objective", &["improve", "planner", "quality"])
            .await
            .expect("set objective");

        let mode_result = handle_slash_command(&mut app, "/objective", &["behavior", "strict"])
            .await
            .expect("set behavior");
        assert_eq!(mode_result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("mode=strict"));
        assert!(output.contains("directives:"));

        let mission_result = handle_slash_command(&mut app, "/objective", &["behavior", "sigma"])
            .await
            .expect("set behavior sigma");
        assert_eq!(mission_result, CommandResult::Handled);
        let mission_output = latest_ui_assistant_text(&app);
        assert!(mission_output.contains("mode=mission"));
    }

    #[tokio::test]
    async fn promoted_subgoal_command_supports_add_update_and_clear() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        app.set_session_objective(Some("stabilize alpha".to_string()));

        handle_subgoal_command(&mut app, &["inspect", "wallet", "drift"]).expect("subgoal add");
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Subgoal checklist"));
        assert!(output.contains("inspect wallet drift"));
        assert!(output.contains("[ ] 1."));

        handle_subgoal_command(&mut app, &["complete", "1"]).expect("subgoal complete");
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Updated subgoal 1 -> completed"));
        assert!(output.contains("[x] 1."));

        handle_subgoal_command(&mut app, &["clear"]).expect("subgoal clear");
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Subgoal checklist cleared."));
    }

    #[tokio::test]
    async fn promoted_handoff_command_surfaces_usage_and_unknown_platform() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_handoff_command(&mut app, &[]).expect("handoff usage");
        let usage = latest_ui_assistant_text(&app);
        assert!(usage.contains("Usage: /handoff <platform>"));

        handle_handoff_command(&mut app, &["pagerduty"]).expect("handoff unknown platform");
        let unknown = latest_ui_assistant_text(&app);
        assert!(unknown.contains("Unknown platform 'pagerduty'"));
    }

    #[test]
    fn test_mission_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/mission"));
        let results = autocomplete("/mis");
        assert!(results.contains(&"/mission"));
    }

    #[test]
    fn test_skins_alias_maps_to_skin() {
        assert_eq!(canonical_command("/skins"), "/skin");
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/skins"));
    }

    #[test]
    fn test_whoami_alias_maps_to_profile() {
        assert_eq!(canonical_command("/whoami"), "/profile");
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/whoami"));
        let results = autocomplete("/who");
        assert!(results.contains(&"/whoami"));
    }

    #[test]
    fn test_resume_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/resume"));
        let results = autocomplete("/res");
        assert!(results.contains(&"/resume"));
    }

    #[test]
    fn test_timetravel_command_and_alias_are_registered() {
        assert!(SLASH_COMMANDS
            .iter()
            .any(|(name, _)| *name == "/timetravel"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/tt"));
        assert_eq!(canonical_command("/tt"), "/timetravel");
        let results = autocomplete("/time");
        assert!(results.contains(&"/timetravel"));
    }

    #[test]
    fn test_simulate_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/simulate"));
        let results = autocomplete("/sim");
        assert!(results.contains(&"/simulate"));
    }

    #[test]
    fn test_qos_and_eval_commands_are_registered() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/qos"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/eval"));
        let qos = autocomplete("/qo");
        assert!(qos.contains(&"/qos"));
        let eval = autocomplete("/eva");
        assert!(eval.contains(&"/eval"));
    }

    #[test]
    fn test_sessions_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/sessions"));
        let results = autocomplete("/sess");
        assert!(results.contains(&"/sessions"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/session"));
        assert_eq!(canonical_command("/session"), "/sessions");
        assert_eq!(canonical_command("/switch"), "/sessions");
        assert!(autocomplete("/sw").contains(&"/switch"));
    }

    #[test]
    fn test_browser_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/browser"));
        let results = autocomplete("/bro");
        assert!(results.contains(&"/browser"));
    }

    #[test]
    fn browser_connect_candidates_prefer_chrome_before_brave_on_linux() {
        let chrome = "/usr/bin/google-chrome";
        let brave = "/usr/bin/brave-browser";
        let candidates = chrome_debug_candidates_with(
            "Linux",
            |_| None,
            |name| match name {
                "google-chrome" => Some(chrome.to_string()),
                "brave-browser" => Some(brave.to_string()),
                _ => None,
            },
            |candidate| candidate == chrome || candidate == brave,
        );

        assert_eq!(candidates[..2], [chrome.to_string(), brave.to_string()]);
        let command =
            manual_chrome_debug_command_with_candidates(9222, "Linux", &candidates).unwrap();
        assert!(command.starts_with("/usr/bin/google-chrome --remote-debugging-port=9222"));
        assert!(command.contains("--no-first-run"));
        assert!(command.contains("--no-default-browser-check"));
    }

    #[test]
    fn browser_connect_candidates_prefer_install_path_before_later_provider_on_path() {
        let chrome = "/opt/google/chrome/chrome";
        let brave = "/usr/bin/brave-browser";
        let candidates = chrome_debug_candidates_with(
            "Linux",
            |_| None,
            |name| (name == "brave-browser").then(|| brave.to_string()),
            |candidate| candidate == chrome || candidate == brave,
        );

        assert_eq!(candidates[..2], [chrome.to_string(), brave.to_string()]);
    }

    #[test]
    fn browser_connect_candidates_include_arch_brave_and_edge_paths() {
        let brave = "/opt/brave-bin/brave";
        let edge = "/usr/bin/microsoft-edge-stable";
        let candidates = chrome_debug_candidates_with(
            "Linux",
            |_| None,
            |_| None,
            |candidate| candidate == brave || candidate == edge,
        );

        assert_eq!(candidates, vec![brave.to_string(), edge.to_string()]);
    }

    #[test]
    fn browser_connect_windows_candidates_prefer_chrome_install_before_brave_path() {
        let chrome = "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";
        let brave = "C:\\Brave\\brave.exe";
        let candidates = chrome_debug_candidates_with(
            "Windows",
            |key| (key == "ProgramFiles").then(|| "C:\\Program Files".to_string()),
            |name| (name == "brave.exe").then(|| brave.to_string()),
            |candidate| candidate == chrome || candidate == brave,
        );

        assert_eq!(candidates[..2], [chrome.to_string(), brave.to_string()]);
        let command =
            manual_chrome_debug_command_with_candidates(9333, "Windows", &candidates).unwrap();
        assert!(command.starts_with("\"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\" --remote-debugging-port=9333"));
        assert!(!command.contains('\''));
    }

    #[test]
    fn browser_connect_manual_command_uses_posix_quoting_for_wsl_paths() {
        let chrome = "/mnt/c/Program Files/Google/Chrome/Application/chrome.exe".to_string();
        let command =
            manual_chrome_debug_command_with_candidates(9222, "Linux", &[chrome]).unwrap();

        assert!(command.starts_with(
            "'/mnt/c/Program Files/Google/Chrome/Application/chrome.exe' --remote-debugging-port=9222"
        ));
    }

    #[test]
    fn browser_probe_base_accepts_websocket_cdp_urls() {
        assert_eq!(
            browser_http_probe_base("ws://127.0.0.1:9222/devtools/browser/abc"),
            "http://127.0.0.1:9222/devtools/browser/abc"
        );
        assert_eq!(
            browser_http_probe_base("wss://example.com/devtools/browser/abc"),
            "https://example.com/devtools/browser/abc"
        );
    }

    #[test]
    fn test_disk_cleanup_command_is_registered_and_completable() {
        assert!(SLASH_COMMANDS
            .iter()
            .any(|(name, _)| *name == "/disk-cleanup"));
        let results = autocomplete("/disk");
        assert!(results.contains(&"/disk-cleanup"));
    }

    #[test]
    fn test_p0_p1_surface_commands_registered_and_completable() {
        for command in [
            "/commands",
            "/boot",
            "/walkthrough",
            "/triage",
            "/subconscious",
            "/integrations",
            "/bundles",
            "/codex-runtime",
            "/platform",
        ] {
            assert!(
                SLASH_COMMANDS.iter().any(|(name, _)| *name == command),
                "missing slash command: {command}"
            );
        }
        assert_eq!(canonical_command("/onboard"), "/walkthrough");
        assert_eq!(canonical_command("/codex_runtime"), "/codex-runtime");
        assert!(autocomplete("/boo").contains(&"/boot"));
        assert!(autocomplete("/wal").contains(&"/walkthrough"));
        assert!(autocomplete("/tri").contains(&"/triage"));
        assert!(autocomplete("/subc").contains(&"/subconscious"));
        assert!(autocomplete("/inte").contains(&"/integrations"));
        assert!(autocomplete("/bun").contains(&"/bundles"));
        assert!(autocomplete("/codex").contains(&"/codex-runtime"));
        assert!(autocomplete("/plat").contains(&"/platform"));
    }

    #[tokio::test]
    async fn p0_walkthrough_and_integrations_commands_emit_expected_sections() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/walkthrough", &["start", "quick"])
            .await
            .expect("walkthrough start");
        let started = latest_ui_assistant_text(&app);
        assert!(started.contains("walkthrough"));
        assert!(started.contains("Use `/walkthrough done <step-id>`"));

        handle_slash_command(&mut app, "/integrations", &["status"])
            .await
            .expect("integrations status");
        let integrations = latest_ui_assistant_text(&app);
        assert!(integrations.contains("Integration Control Plane"));
        assert!(integrations.contains("provider:"));
    }

    #[tokio::test]
    async fn upstream_parity_bundles_command_lists_skill_bundles() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let bundle_dir = app.state_root.join("skill-bundles");
        std::fs::create_dir_all(&bundle_dir).expect("create bundle dir");
        std::fs::write(
            bundle_dir.join("review-pack.yaml"),
            "name: Review Pack\ndescription: Review plus Rust checks\nskills:\n  - code-review\n  - rust-code-quality\n",
        )
        .expect("write bundle");

        handle_slash_command(&mut app, "/bundles", &[])
            .await
            .expect("bundles");
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Skill Bundles (1 installed):"));
        assert!(output.contains("/review-pack"));
        assert!(output.contains("code-review"));
    }

    #[tokio::test]
    async fn upstream_parity_codex_runtime_persists_auto_without_codex_binary() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        std::fs::write(
            app.state_root.join("config.yaml"),
            "model:\n  provider: openai\n  default: dynamic\n  openai_runtime: codex_app_server\n",
        )
        .expect("write config");

        handle_slash_command(&mut app, "/codex-runtime", &["auto"])
            .await
            .expect("codex runtime");
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("openai_runtime: codex_app_server -> auto"));
        let updated = std::fs::read_to_string(app.state_root.join("config.yaml")).expect("config");
        assert!(updated.contains("openai_runtime: auto"));
    }

    #[tokio::test]
    async fn upstream_parity_platform_command_lists_cli_platform_state() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let mut config = GatewayConfig::default();
        config.platforms.insert(
            "telegram".to_string(),
            hermes_config::PlatformConfig {
                enabled: true,
                token: Some("secret-token".to_string()),
                ..Default::default()
            },
        );
        app.config = std::sync::Arc::new(config);

        handle_slash_command(&mut app, "/platform", &["list"])
            .await
            .expect("platform list");
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Gateway platforms"));
        assert!(output.contains("telegram: enabled=true token=configured"));
        assert!(!output.contains("secret-token"));

        handle_slash_command(&mut app, "/platform", &["pause", "telegram"])
            .await
            .expect("platform pause");
        assert!(latest_ui_assistant_text(&app).contains("running gateway process"));
    }

    #[tokio::test]
    async fn p0_compress_rules_set_and_apply_updates_runtime_env() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        std::env::remove_var("HERMES_TUI_MAX_ASSISTANT_RENDER_LINES");

        handle_slash_command(
            &mut app,
            "/compress",
            &["rules", "set", "user", "assistant_lines", "320"],
        )
        .await
        .expect("compress rules set user");
        let set_output = latest_ui_assistant_text(&app);
        assert!(set_output.contains("Updated user compression rule"));
        assert!(set_output.contains("assistant_lines=320"));

        handle_slash_command(&mut app, "/compress", &["rules", "apply"])
            .await
            .expect("compress rules apply");
        let applied = latest_ui_assistant_text(&app);
        assert!(applied.contains("Applied compression policy to runtime env"));
        assert_eq!(
            std::env::var("HERMES_TUI_MAX_ASSISTANT_RENDER_LINES")
                .ok()
                .as_deref(),
            Some("320")
        );
    }

    #[tokio::test]
    async fn compress_command_reports_before_after_message_and_token_summary() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        app.messages = (0..6)
            .map(|idx| hermes_core::Message::user(format!("message-{idx} {}", "x".repeat(80))))
            .collect();

        handle_slash_command(&mut app, "/compress", &[])
            .await
            .expect("compress command");

        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Compressed:"));
        assert!(output.contains("6 → 3 messages"));
        assert!(output.contains("tokens"));
        assert!(output.contains("removed 4 messages, kept 2"));
    }
