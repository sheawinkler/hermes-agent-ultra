#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock;
    use clap::Parser;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    struct TempHomeGuard {
        previous_home: Option<String>,
        previous_clipboard_mock: Option<String>,
        previous_runtime_env: Vec<(&'static str, Option<String>)>,
    }

    impl TempHomeGuard {
        fn new(path: &Path) -> Self {
            let previous_home = std::env::var("HERMES_HOME").ok();
            std::env::set_var("HERMES_HOME", path);
            let previous_clipboard_mock = std::env::var("HERMES_TEST_CLIPBOARD_TEXT").ok();
            std::env::remove_var("HERMES_TEST_CLIPBOARD_TEXT");
            let previous_runtime_env = [
                "HERMES_MODEL",
                "HERMES_INFERENCE_MODEL",
                "HERMES_INFERENCE_PROVIDER",
                "HERMES_TUI_PROVIDER",
                "HERMES_TUI_MAX_ASSISTANT_RENDER_LINES",
                "HERMES_TUI_MAX_TOOL_OUTPUT_LINES",
                "HERMES_TUI_MAX_TOOL_OUTPUT_LINE_CHARS",
                "HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS",
            ]
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
            Self {
                previous_home,
                previous_clipboard_mock,
                previous_runtime_env,
            }
        }
    }

    impl Drop for TempHomeGuard {
        fn drop(&mut self) {
            match self.previous_home.take() {
                Some(value) => std::env::set_var("HERMES_HOME", value),
                None => std::env::remove_var("HERMES_HOME"),
            }
            match self.previous_clipboard_mock.take() {
                Some(value) => std::env::set_var("HERMES_TEST_CLIPBOARD_TEXT", value),
                None => std::env::remove_var("HERMES_TEST_CLIPBOARD_TEXT"),
            }
            for (key, value) in self.previous_runtime_env.drain(..) {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn write_test_executable(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, b"#!/bin/sh\n").expect("write executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod executable");
        }
    }

    struct ReasoningFullResetGuard;

    impl ReasoningFullResetGuard {
        fn new() -> Self {
            set_reasoning_full(false);
            Self
        }
    }

    impl Drop for ReasoningFullResetGuard {
        fn drop(&mut self) {
            set_reasoning_full(false);
        }
    }

    async fn build_test_app_with_stream(home: &Path) -> App {
        let config_dir = home.join("config");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let cli = crate::cli::Cli::try_parse_from(vec![
            "hermes".to_string(),
            "-C".to_string(),
            config_dir.display().to_string(),
            "--ignore-user-config".to_string(),
            "--ignore-rules".to_string(),
        ])
        .expect("parse cli");
        let mut app = App::new(cli).await.expect("build app");
        let (tx, _rx) = mpsc::unbounded_channel::<crate::tui::Event>();
        app.set_stream_handle(Some(tx.into()));
        app
    }

    fn latest_ui_assistant_text(app: &App) -> String {
        app.ui_messages
            .iter()
            .rev()
            .find(|row| row.message.role == hermes_core::MessageRole::Assistant)
            .and_then(|row| row.message.content.clone())
            .unwrap_or_default()
    }

    fn insert_quick_command(app: &mut App, name: &str, command: hermes_config::QuickCommandConfig) {
        let mut config = (*app.config).clone();
        config.quick_commands.insert(name.to_string(), command);
        app.config = Arc::new(config);
    }

    #[tokio::test]
    async fn external_plugin_command_rejects_python_dispatch() {
        let err = handle_cli_external_plugin_subcommand(vec!["honcho".to_string()])
            .await
            .expect_err("external plugin commands should be rejected");
        assert!(err
            .to_string()
            .contains("Python plugin command dispatch is disabled"));
    }

    fn tar_gz_entry_names(path: &Path) -> Vec<String> {
        let file = std::fs::File::open(path).expect("open archive");
        let dec = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(dec);
        archive
            .entries()
            .expect("entries")
            .filter_map(|entry| {
                entry
                    .ok()
                    .and_then(|entry| entry.path().ok().map(|path| path.to_string_lossy().into()))
            })
            .collect()
    }

    #[test]
    fn backup_restore_target_rejects_absolute_and_parent_paths() {
        let hermes = Path::new("/tmp/hermes");
        let home = Path::new("/tmp/home");

        assert!(restore_archive_entry_target(Path::new("../escape"), hermes, home).is_none());
        assert!(
            restore_archive_entry_target(Path::new("external/../escape"), hermes, home).is_none()
        );
        assert_eq!(
            restore_archive_entry_target(Path::new("hermes/config.yaml"), hermes, home),
            Some(hermes.join("config.yaml"))
        );
        assert_eq!(
            restore_archive_entry_target(Path::new("external/.openviking/auth.json"), hermes, home),
            Some(home.join(".openviking/auth.json"))
        );
    }

    #[tokio::test]
    async fn cli_backup_import_strips_hermes_archive_prefix() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("hermes-home");
        let fake_home = tmp.path().join("home");
        std::fs::create_dir_all(&hermes_home).expect("hermes home");
        std::fs::create_dir_all(&fake_home).expect("fake home");
        std::fs::write(hermes_home.join("config.yaml"), "model: dynamic\n").expect("write config");
        let _home_guard = TempHomeGuard::new(&hermes_home);
        let _unix_home = EnvVarGuard::set("HOME", &fake_home);
        let _openviking_endpoint = EnvVarGuard::remove("OPENVIKING_ENDPOINT");
        let _hindsight_mode = EnvVarGuard::remove("HINDSIGHT_MODE");
        let archive = tmp.path().join("backup.tar.gz");

        handle_cli_backup(Some(archive.to_string_lossy().to_string()))
            .await
            .expect("backup");
        std::fs::remove_file(hermes_home.join("config.yaml")).expect("remove config");
        handle_cli_import(archive.to_string_lossy().to_string())
            .await
            .expect("import");

        assert_eq!(
            std::fs::read_to_string(hermes_home.join("config.yaml")).expect("restored config"),
            "model: dynamic\n"
        );
        assert!(!hermes_home.join("hermes/config.yaml").exists());
    }

    #[tokio::test]
    async fn cli_backup_import_round_trips_openviking_external_state() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("hermes-home");
        let fake_home = tmp.path().join("home");
        let openviking_dir = fake_home.join(".openviking");
        std::fs::create_dir_all(&hermes_home).expect("hermes home");
        std::fs::create_dir_all(&openviking_dir).expect("openviking dir");
        std::fs::write(
            hermes_home.join("openviking.json"),
            r#"{"enabled":true,"endpoint":"http://127.0.0.1:1933"}"#,
        )
        .expect("write openviking config");
        std::fs::write(openviking_dir.join("auth.json"), r#"{"token":"secret"}"#)
            .expect("write auth");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            openviking_dir.join("auth.json"),
            openviking_dir.join("link.json"),
        )
        .expect("symlink");
        let _home_guard = TempHomeGuard::new(&hermes_home);
        let _unix_home = EnvVarGuard::set("HOME", &fake_home);
        let _openviking_endpoint = EnvVarGuard::remove("OPENVIKING_ENDPOINT");
        let _openviking_key = EnvVarGuard::remove("OPENVIKING_API_KEY");
        let archive = tmp.path().join("backup.tar.gz");

        handle_cli_backup(Some(archive.to_string_lossy().to_string()))
            .await
            .expect("backup");

        let entries = tar_gz_entry_names(&archive);
        assert!(entries
            .iter()
            .any(|entry| entry == "external/.openviking/auth.json"));
        assert!(!entries
            .iter()
            .any(|entry| entry == "external/.openviking/link.json"));

        std::fs::remove_dir_all(&openviking_dir).expect("remove external");
        handle_cli_import(archive.to_string_lossy().to_string())
            .await
            .expect("import");

        let restored = openviking_dir.join("auth.json");
        assert_eq!(
            std::fs::read_to_string(&restored).expect("restored auth"),
            r#"{"token":"secret"}"#
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&restored)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn memory_honcho_setup_config_stores_local_jwt_under_host_block() {
        let aliases = serde_json::Map::new();
        let cfg = build_honcho_setup_config(HonchoSetupConfigInput {
            host: "hermes",
            deployment: "local",
            api_key: "local-jwt",
            base_url: "http://localhost:8000",
            peer_name: "operator",
            shape: "single",
            runtime_peer_prefix: "",
            aliases: &aliases,
        });

        assert_eq!(cfg["baseUrl"], "http://localhost:8000");
        assert!(cfg.get("apiKey").is_none());
        assert_eq!(cfg["hosts"]["hermes"]["apiKey"], "local-jwt");
        assert!(cfg["hosts"]["hermes"]["pinUserPeer"]
            .as_bool()
            .expect("hermes pinUserPeer bool"));
    }

    #[test]
    fn memory_honcho_setup_config_keeps_gateway_aliases_for_hybrid_shape() {
        let aliases = parse_honcho_aliases("telegram-1=operator, discord-2=operator");
        let cfg = build_honcho_setup_config(HonchoSetupConfigInput {
            host: "hermes_coder",
            deployment: "cloud",
            api_key: "cloud-key",
            base_url: "",
            peer_name: "operator",
            shape: "hybrid",
            runtime_peer_prefix: "gateway_",
            aliases: &aliases,
        });

        assert_eq!(cfg["apiKey"], "cloud-key");
        assert_eq!(cfg["hosts"]["hermes_coder"]["aiPeer"], "coder");
        assert!(!cfg["hosts"]["hermes_coder"]["pinUserPeer"]
            .as_bool()
            .expect("hermes_coder pinUserPeer bool"));
        assert_eq!(
            cfg["hosts"]["hermes_coder"]["userPeerAliases"]["telegram-1"],
            "operator"
        );
        assert_eq!(
            cfg["hosts"]["hermes_coder"]["runtimePeerPrefix"],
            "gateway_"
        );
    }

    #[test]
    fn memory_honcho_profile_host_key_is_honcho_safe() {
        let _guard = env_test_lock();
        let prev_profile = std::env::var("HERMES_PROFILE").ok();
        let prev_host = std::env::var("HERMES_HONCHO_HOST").ok();
        std::env::set_var("HERMES_PROFILE", "research.team/v1");
        std::env::remove_var("HERMES_HONCHO_HOST");

        assert_eq!(active_honcho_host_key_for_cli(), "hermes_research_team_v1");
        assert_eq!(
            honcho_ai_peer_for_host("hermes_research_team_v1"),
            "research_team_v1"
        );

        match prev_profile {
            Some(value) => std::env::set_var("HERMES_PROFILE", value),
            None => std::env::remove_var("HERMES_PROFILE"),
        }
        match prev_host {
            Some(value) => std::env::set_var("HERMES_HONCHO_HOST", value),
            None => std::env::remove_var("HERMES_HONCHO_HOST"),
        }
    }

    #[test]
    fn memory_honcho_setup_accepts_and_preserves_existing_oauth_grant() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home = TempHomeGuard::new(tmp.path());
        let _profile = EnvVarGuard::remove("HERMES_PROFILE");
        let _host = EnvVarGuard::remove("HERMES_HONCHO_HOST");
        let _api_key = EnvVarGuard::remove("HONCHO_API_KEY");
        let _base_url = EnvVarGuard::remove("HONCHO_BASE_URL");
        let path = tmp.path().join("honcho.json");
        std::fs::write(
            &path,
            r#"{
                "enabled": true,
                "hosts": {
                    "hermes": {
                        "apiKey": "hch-at-existing",
                        "oauth": {
                            "refreshToken": "hch-rt-existing",
                            "expiresAt": 9999999999,
                            "clientId": "hermes-agent",
                            "tokenEndpoint": "https://api.honcho.dev/oauth/token"
                        }
                    }
                }
            }"#,
        )
        .expect("write config");

        let saved_path = setup_memory_provider_target("honcho", true).expect("setup honcho");

        assert_eq!(saved_path, path);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&saved_path).expect("read config"))
                .expect("json");
        assert_eq!(parsed["hosts"]["hermes"]["apiKey"], "hch-at-existing");
        assert_eq!(
            parsed["hosts"]["hermes"]["oauth"]["refreshToken"],
            "hch-rt-existing"
        );
        assert!(parsed["hosts"]["hermes"]["pinUserPeer"]
            .as_bool()
            .expect("hermes pinUserPeer bool"));
    }

    #[test]
    fn memory_openviking_setup_config_normalizes_local_no_key_mode() {
        let cfg = build_openviking_setup_config(OpenVikingSetupConfigInput {
            endpoint: "localhost:1933/",
            api_key: "",
            api_key_type: "none",
            account: "",
            user: "",
            agent: "",
        })
        .expect("local setup config");

        assert!(cfg["enabled"].as_bool().expect("enabled bool"));
        assert_eq!(cfg["endpoint"], "http://localhost:1933");
        assert_eq!(cfg["api_key"], "");
        assert_eq!(cfg["api_key_type"], "none");
        assert_eq!(cfg["account"], "default");
        assert_eq!(cfg["user"], "default");
        assert_eq!(cfg["agent"], "hermes");
    }

    #[test]
    fn memory_openviking_setup_config_requires_root_tenant_identity() {
        let err = build_openviking_setup_config(OpenVikingSetupConfigInput {
            endpoint: "https://openviking.example",
            api_key: "root-secret",
            api_key_type: "root",
            account: "",
            user: "operator",
            agent: "hermes",
        })
        .expect_err("missing account should fail");

        assert!(err.to_string().contains("requires account and user"));
    }

    #[test]
    fn memory_openviking_setup_config_supports_user_key_without_tenant_prompts() {
        let cfg = build_openviking_setup_config(OpenVikingSetupConfigInput {
            endpoint: "https://openviking.example/",
            api_key: "user-secret",
            api_key_type: "user",
            account: "",
            user: "",
            agent: "agent",
        })
        .expect("user setup config");

        assert_eq!(cfg["endpoint"], "https://openviking.example");
        assert_eq!(cfg["api_key_type"], "user");
        assert_eq!(cfg["api_key"], "user-secret");
        assert_eq!(cfg["account"], "default");
        assert_eq!(cfg["user"], "default");
        assert_eq!(cfg["agent"], "agent");
    }

    #[test]
    fn memory_openviking_setup_target_writes_owner_only_config() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home = TempHomeGuard::new(tmp.path());
        let _endpoint = EnvVarGuard::set("OPENVIKING_ENDPOINT", "http://localhost:1933");
        let _api_key = EnvVarGuard::remove("OPENVIKING_API_KEY");
        let _key_type = EnvVarGuard::remove("OPENVIKING_API_KEY_TYPE");
        let _account = EnvVarGuard::remove("OPENVIKING_ACCOUNT");
        let _user = EnvVarGuard::remove("OPENVIKING_USER");
        let _agent = EnvVarGuard::remove("OPENVIKING_AGENT");

        let path = setup_memory_provider_target("openviking", true).expect("setup openviking");

        assert_eq!(path, tmp.path().join("openviking.json"));
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("json");
        assert!(parsed["enabled"].as_bool().expect("enabled bool"));
        assert_eq!(parsed["endpoint"], "http://localhost:1933");
        assert_eq!(parsed["api_key_type"], "none");
        assert_eq!(parsed["agent"], "hermes");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn memory_setup_prompt_classifies_secret_labels() {
        assert!(memory_setup_label_is_secret("Mem0 API key"));
        assert!(memory_setup_label_is_secret("Honcho local JWT/API key"));
        assert!(memory_setup_label_is_secret("OpenViking root API key"));
        assert!(!memory_setup_label_is_secret("Mem0 base_url"));
        assert!(!memory_setup_label_is_secret("Deployment shape"));
    }

    #[test]
    fn test_autocomplete_empty() {
        let results = autocomplete("");
        assert_eq!(results.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn test_autocomplete_partial() {
        let results = autocomplete("/m");
        assert!(results.contains(&"/model"));
    }

    #[test]
    fn test_contextual_autocomplete_swarm_subcommands() {
        let results = autocomplete_contextual("/swarm ");
        assert!(results.contains(&"/swarm status ".to_string()));
        assert!(results.contains(&"/swarm run ".to_string()));
    }

    #[test]
    fn test_contextual_autocomplete_swarm_nested_modes() {
        let results = autocomplete_contextual("/swarm plan ");
        assert!(results.contains(&"/swarm plan graph ".to_string()));
        assert!(results.contains(&"/swarm plan sequential ".to_string()));
    }

    #[test]
    fn test_contextual_autocomplete_objective_behavior_modes() {
        let results = autocomplete_contextual("/objective behavior ");
        assert!(results.contains(&"/objective behavior strict ".to_string()));
        assert!(results.contains(&"/objective behavior sigma ".to_string()));
    }

    #[test]
    fn test_contextual_autocomplete_personality_candidates() {
        let results = autocomplete_contextual("/personality ");
        assert!(results.contains(&"/personality coder ".to_string()));
        assert!(results.contains(&"/personality none ".to_string()));
    }

    #[tokio::test]
    async fn version_slash_command_renders_shared_version_label() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/version", &[])
            .await
            .expect("version command");

        assert_eq!(result, CommandResult::Handled);
        assert_eq!(
            latest_ui_assistant_text(&app),
            hermes_core::version::version_label()
        );
    }

    #[tokio::test]
    async fn reasoning_full_and_clamp_commands_update_mode_and_status() {
        let _guard = env_test_lock();
        let _reasoning_guard = ReasoningFullResetGuard::new();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/reasoning", &["full"])
            .await
            .expect("reasoning full command");
        assert_eq!(result, CommandResult::Handled);
        assert!(reasoning_full_enabled());
        assert!(latest_ui_assistant_text(&app).contains("Reasoning mode: full"));

        handle_slash_command(&mut app, "/reasoning", &["status"])
            .await
            .expect("reasoning status command");
        let status = latest_ui_assistant_text(&app);
        assert!(status.contains("- display: OFF"));
        assert!(status.contains("- mode: full"));
        assert!(status.contains("- effort: auto"));

        handle_slash_command(&mut app, "/reasoning", &["help"])
            .await
            .expect("reasoning help command");
        assert!(latest_ui_assistant_text(&app).contains("/reasoning full|clamp"));

        handle_slash_command(&mut app, "/reasoning", &["clamp"])
            .await
            .expect("reasoning clamp command");
        assert!(!reasoning_full_enabled());
        assert!(latest_ui_assistant_text(&app).contains("Reasoning mode: clamp"));
    }

    #[tokio::test]
    async fn quick_exec_command_prints_stdout_before_agent_loop() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        insert_quick_command(
            &mut app,
            "dn",
            hermes_config::QuickCommandConfig {
                kind: "exec".to_string(),
                command: Some("printf daily-note".to_string()),
                ..Default::default()
            },
        );

        let result = handle_slash_command(&mut app, "/dn", &[])
            .await
            .expect("quick command");

        assert_eq!(result, CommandResult::Handled);
        assert_eq!(latest_ui_assistant_text(&app), "daily-note");
    }

    #[tokio::test]
    async fn quick_exec_no_output_and_timeout_have_user_visible_replies() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        insert_quick_command(
            &mut app,
            "empty",
            hermes_config::QuickCommandConfig {
                kind: "exec".to_string(),
                command: Some("true".to_string()),
                ..Default::default()
            },
        );
        insert_quick_command(
            &mut app,
            "slow",
            hermes_config::QuickCommandConfig {
                kind: "exec".to_string(),
                command: Some("sleep 1".to_string()),
                timeout_secs: Some(0),
                ..Default::default()
            },
        );

        handle_slash_command(&mut app, "/empty", &[])
            .await
            .expect("empty command");
        assert!(latest_ui_assistant_text(&app).contains("no output"));

        handle_slash_command(&mut app, "/slow", &[])
            .await
            .expect("timeout command");
        assert!(latest_ui_assistant_text(&app).contains("timed out"));
    }

    #[tokio::test]
    async fn quick_alias_rewrites_to_builtin_and_passes_args() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        insert_quick_command(
            &mut app,
            "sc",
            hermes_config::QuickCommandConfig {
                kind: "alias".to_string(),
                target: Some("/queue".to_string()),
                ..Default::default()
            },
        );

        handle_slash_command(&mut app, "/sc", &["some", "args"])
            .await
            .expect("alias command");

        assert!(latest_ui_assistant_text(&app).contains("some args"));
    }

    #[tokio::test]
    async fn blueprint_slash_command_creates_cron_job() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(
            &mut app,
            "/bp",
            &[
                "custom-reminder",
                "what=\"drink",
                "water\"",
                "time=10:15",
                "recurrence=weekdays",
                "deliver=local",
            ],
        )
        .await
        .expect("blueprint command");

        assert_eq!(result, CommandResult::Handled);
        let jobs = app.cron_scheduler.list_jobs().await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name.as_deref(), Some("Custom reminder"));
        assert_eq!(jobs[0].schedule, "15 10 * * 1-5");
        assert_eq!(jobs[0].prompt, "Remind the user: drink water");
        assert_eq!(
            jobs[0].deliver.as_ref().map(|d| &d.target),
            Some(&DeliverTarget::Local)
        );
        assert!(latest_ui_assistant_text(&app).contains("Scheduled `Custom reminder`"));
    }

    #[tokio::test]
    async fn app_contextual_autocomplete_and_tools_toggle_use_runtime_state() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        {
            let config = Arc::make_mut(&mut app.config);
            config.platforms.insert(
                "telegram".to_string(),
                hermes_config::PlatformConfig {
                    enabled: true,
                    ..Default::default()
                },
            );
            config.tools_config.disabled.push("read_file".to_string());
        }
        app.tool_schemas = hermes_tool_planning::resolve_platform_tool_schemas(
            &app.config,
            "cli",
            &app.tool_registry,
        );

        let handoff = autocomplete_contextual_for_app("/handoff ", &app);
        assert!(handoff.contains(&"/handoff telegram ".to_string()));
        let enable = autocomplete_contextual_for_app("/tools enable ", &app);
        assert!(enable.contains(&"/tools enable read_file ".to_string()));
        let disable = autocomplete_contextual_for_app("/tools disable ", &app);
        assert!(disable.contains(&"/tools disable write_file ".to_string()));
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "read_file"));

        handle_slash_command(&mut app, "/tools", &["enable", "read_file"])
            .await
            .expect("enable tool");
        assert!(latest_ui_assistant_text(&app).contains("Enabled tool `read_file`"));
        assert!(app
            .config
            .tools_config
            .enabled
            .iter()
            .any(|name| name == "read_file"));
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "read_file"));

        handle_slash_command(&mut app, "/tools", &["disable", "read_file"])
            .await
            .expect("disable tool");
        assert!(latest_ui_assistant_text(&app).contains("Disabled tool `read_file`"));
        assert!(app
            .config
            .tools_config
            .disabled
            .iter()
            .any(|name| name == "read_file"));
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "read_file"));
    }

    #[tokio::test]
    async fn quick_command_takes_priority_over_builtin_slash_command() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        insert_quick_command(
            &mut app,
            "help",
            hermes_config::QuickCommandConfig {
                kind: "exec".to_string(),
                command: Some("printf overridden".to_string()),
                ..Default::default()
            },
        );

        handle_slash_command(&mut app, "/help", &[])
            .await
            .expect("overridden help");

        assert_eq!(latest_ui_assistant_text(&app), "overridden");
    }

    #[tokio::test]
    async fn cli_resolves_installed_skill_slash_command() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let skill_dir = tmp.path().join("skills").join("release-captain");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Release Captain\ndescription: Release workflow\n---\n# Release Captain\n1. Inspect changed files\n2. Run deterministic gates\n",
        )
        .expect("write skill");
        let app = build_test_app_with_stream(tmp.path()).await;

        let invocation = resolve_cli_skill_slash_command(&app, "/release_captain", &["ship", "it"])
            .expect("resolve skill command")
            .expect("skill command");

        assert_eq!(invocation.command, "/release-captain");
        assert_eq!(invocation.skill_name, "Release Captain");
        assert!(invocation.message.contains("Inspect changed files"));
        assert!(invocation.message.contains("ship it"));
    }

    #[test]
    fn local_skill_summaries_include_categorized_skills() {
        let tmp = tempdir().expect("tempdir");
        let skills_dir = tmp.path().join("skills");
        let google_skill_dir = skills_dir.join("productivity").join("google-workspace");
        std::fs::create_dir_all(&google_skill_dir).expect("create categorized skill dir");
        std::fs::write(
            google_skill_dir.join("SKILL.md"),
            "---\nname: google-workspace\ndescription: Google Workspace automation\n---\n# Google Workspace\n",
        )
        .expect("write categorized skill");

        let root_skill_dir = skills_dir.join("release-captain");
        std::fs::create_dir_all(&root_skill_dir).expect("create root skill dir");
        std::fs::write(
            root_skill_dir.join("SKILL.md"),
            "---\nname: release-captain\ndescription: Release workflow\n---\n# Release Captain\n",
        )
        .expect("write root skill");

        let summaries = collect_local_skill_summaries(&skills_dir);
        assert!(summaries.iter().any(|summary| {
            summary.name == "google-workspace"
                && summary.relative_dir == "productivity/google-workspace"
                && summary.title == "Google Workspace automation"
        }));
        assert!(summaries
            .iter()
            .any(|summary| summary.name == "release-captain"));
    }

    #[test]
    fn local_skill_markdown_resolves_by_name_and_relative_path() {
        let tmp = tempdir().expect("tempdir");
        let skills_dir = tmp.path().join("skills");
        let google_skill_dir = skills_dir.join("productivity").join("google-workspace");
        std::fs::create_dir_all(&google_skill_dir).expect("create categorized skill dir");
        let skill_md = google_skill_dir.join("SKILL.md");
        std::fs::write(
            &skill_md,
            "---\nname: google-workspace\ndescription: Google Workspace automation\n---\n# Google Workspace\n",
        )
        .expect("write categorized skill");

        assert_eq!(
            find_local_skill_markdown(&skills_dir, "google-workspace").as_deref(),
            Some(skill_md.as_path())
        );
        assert_eq!(
            find_local_skill_markdown(&skills_dir, "productivity/google-workspace").as_deref(),
            Some(skill_md.as_path())
        );
    }

    #[tokio::test]
    async fn cli_reload_skills_reports_snapshot_and_queues_note() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let skill_dir = tmp.path().join("skills").join("release-captain");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Release Captain\ndescription: Release workflow\n---\n# Release Captain\n1. Inspect changed files\n",
        )
        .expect("write skill");
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/reload-skills", &[])
            .await
            .expect("reload skills");

        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Reloaded installed skill commands"));
        assert!(out.contains("/release-captain"));
        assert!(out.contains("no prompt cache was invalidated"));
        assert_eq!(app.pending_system_note_count(), 1);
    }

    #[tokio::test]
    async fn promoted_snapshot_command_lists_snapshots() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_snapshot_command(&mut app, &[]).expect("snapshot list");
        assert_eq!(result, CommandResult::Handled);

        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Session snapshots:") || output.contains("No snapshots found in"));
    }

    #[tokio::test]
    async fn promoted_rollback_command_shows_controls() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_rollback_command(&mut app, &[]).expect("rollback list");
        assert_eq!(result, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Rollback controls:"));
    }

    #[tokio::test]
    async fn promoted_queue_command_shows_usage_and_status() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let usage = handle_queue_command(&mut app, &[]).expect("queue usage");
        assert_eq!(usage, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Usage: /queue <prompt>"));

        let status = handle_queue_command(&mut app, &["status"]).expect("queue status");
        assert_eq!(status, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Background queue status:"));
    }

    #[tokio::test]
    async fn promoted_steer_command_sets_and_clears_instruction() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_steer_command(&mut app, &["focus", "on", "repo", "map"]).expect("set steer");
        assert_eq!(
            current_session_steer(&app).as_deref(),
            Some("focus on repo map")
        );
        assert!(latest_ui_assistant_text(&app).contains("Steering instruction set."));

        handle_steer_command(&mut app, &["clear"]).expect("clear steer");
        assert!(current_session_steer(&app).is_none());
        assert!(latest_ui_assistant_text(&app).contains("Cleared session steering instruction."));
    }

    #[test]
    fn acp_steer_prompt_interrupts_with_trusted_marker() {
        let session_id = "session-steer-marker".to_string();
        let controller = hermes_agent::InterruptController::new();
        let interrupts = Arc::new(Mutex::new(HashMap::from([(
            session_id.clone(),
            controller.clone(),
        )])));
        let executor = CliAcpPromptExecutor {
            config: Arc::new(GatewayConfig::default()),
            tool_registry: Arc::new(hermes_tools::ToolRegistry::new()),
            interrupts,
        };
        let session = hermes_acp::SessionState::new(session_id, ".".to_string());

        assert!(hermes_acp::AcpPromptExecutor::steer_prompt(
            &executor,
            &session,
            "prefer the simpler fix"
        )
        .expect("steer prompt"));

        let marker = controller
            .take_interrupt_graceful()
            .expect("interrupt set")
            .expect("marker");
        assert!(marker.contains(hermes_agent::STEER_MARKER_OPEN));
        assert!(marker.contains("prefer the simpler fix"));
        assert!(marker.contains(hermes_agent::STEER_MARKER_CLOSE));
        assert!(!marker.contains("User guidance:"));
    }

    struct CliNoopTool {
        schema: hermes_core::ToolSchema,
    }

    #[async_trait::async_trait]
    impl hermes_core::ToolHandler for CliNoopTool {
        async fn execute(
            &self,
            _params: serde_json::Value,
        ) -> Result<String, hermes_core::ToolError> {
            Ok("ok".to_string())
        }

        fn schema(&self) -> hermes_core::ToolSchema {
            self.schema.clone()
        }
    }

    fn register_cli_noop_tool(registry: &hermes_tools::ToolRegistry, name: &str) {
        let schema =
            hermes_core::tool_schema(name, "CLI noop", hermes_core::JsonSchema::new("object"));
        registry.register(
            name,
            "mcp-test",
            schema.clone(),
            Arc::new(CliNoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "CLI noop",
            "mcp",
            None,
        );
    }

    #[tokio::test]
    async fn reload_mcp_refreshes_agent_snapshot_from_runtime_registry() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        assert!(app.agent.tool_registry.get("mcp_srv_ping").is_none());
        register_cli_noop_tool(&app.tool_registry, "mcp_srv_ping");

        let result = handle_reload_command(&mut app, "/reload-mcp").expect("reload mcp");

        assert_eq!(result, CommandResult::Handled);
        assert!(app.agent.tool_registry.get("mcp_srv_ping").is_some());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("MCP reload complete"));
        assert!(out.contains("Added: mcp_srv_ping"));
    }

    #[test]
    fn acp_prompt_executor_respects_mcp_toolset_gate() {
        let mut config = GatewayConfig::default();
        config
            .platform_toolsets
            .insert("cli".to_string(), vec!["file".to_string()]);
        let tool_registry = Arc::new(hermes_tools::ToolRegistry::new());
        let executor = CliAcpPromptExecutor {
            config: Arc::new(config),
            tool_registry: Arc::clone(&tool_registry),
            interrupts: Arc::new(Mutex::new(HashMap::new())),
        };
        let file_schema = hermes_core::tool_schema(
            "read_file",
            "Read file",
            hermes_core::JsonSchema::new("object"),
        );
        tool_registry.register(
            "read_file",
            "file",
            file_schema.clone(),
            Arc::new(CliNoopTool {
                schema: file_schema,
            }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "Read file",
            "file",
            None,
        );
        assert!(executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "read_file"));
        assert!(!executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));

        let schema = hermes_core::tool_schema(
            "mcp_srv_ping",
            "MCP ping",
            hermes_core::JsonSchema::new("object"),
        );
        tool_registry.register(
            "mcp_srv_ping",
            "mcp-srv",
            schema.clone(),
            Arc::new(CliNoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "MCP ping",
            "mcp",
            None,
        );

        assert!(!executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));
    }

    #[test]
    fn acp_prompt_executor_allows_explicit_mcp_toolset_alias() {
        let mut config = GatewayConfig::default();
        config
            .platform_toolsets
            .insert("cli".to_string(), vec!["srv".to_string()]);
        let tool_registry = Arc::new(hermes_tools::ToolRegistry::new());
        let executor = CliAcpPromptExecutor {
            config: Arc::new(config),
            tool_registry: Arc::clone(&tool_registry),
            interrupts: Arc::new(Mutex::new(HashMap::new())),
        };
        let schema = hermes_core::tool_schema(
            "mcp_srv_ping",
            "MCP ping",
            hermes_core::JsonSchema::new("object"),
        );
        tool_registry.register(
            "mcp_srv_ping",
            "mcp-srv",
            schema.clone(),
            Arc::new(CliNoopTool { schema }),
            Arc::new(|| true),
            Vec::new(),
            true,
            "MCP ping",
            "mcp",
            None,
        );
        tool_registry.register_toolset_alias("srv", "mcp-srv");

        assert!(executor
            .current_tool_schemas()
            .iter()
            .any(|schema| schema.name == "mcp_srv_ping"));
    }

    #[tokio::test]
    async fn promoted_btw_command_queues_ephemeral_background_task() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result =
            handle_btw_command(&mut app, &["why", "is", "latency", "high?"]).expect("btw command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("[/btw queued]"));
        assert!(output.contains("Question: why is latency high?"));
    }

    #[tokio::test]
    async fn slash_auth_status_command_is_handled() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/auth", &["status"])
            .await
            .expect("auth status");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Auth status"));
    }

    #[tokio::test]
    async fn slash_runbook_and_telemetry_commands_are_handled() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let runbook = handle_slash_command(&mut app, "/runbook", &["list"])
            .await
            .expect("runbook list");
        assert_eq!(runbook, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Runbooks"));

        let telemetry = handle_slash_command(&mut app, "/telemetry", &["status"])
            .await
            .expect("telemetry status");
        assert_eq!(telemetry, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Telemetry snapshot"));
    }

    #[tokio::test]
    async fn slash_agents_pause_resume_and_status_are_handled() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        std::env::remove_var("HERMES_DELEGATION_PAUSED");

        let status = handle_slash_command(&mut app, "/agents", &["status"])
            .await
            .expect("agents status");
        assert_eq!(status, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Delegation spawning: active"));

        let pause = handle_slash_command(&mut app, "/agents", &["pause"])
            .await
            .expect("agents pause");
        assert_eq!(pause, CommandResult::Handled);
        assert_eq!(
            std::env::var("HERMES_DELEGATION_PAUSED").ok().as_deref(),
            Some("1")
        );
        assert!(latest_ui_assistant_text(&app).contains("paused for this runtime"));

        let resume = handle_slash_command(&mut app, "/agents", &["resume"])
            .await
            .expect("agents resume");
        assert_eq!(resume, CommandResult::Handled);
        assert_eq!(
            std::env::var("HERMES_DELEGATION_PAUSED").ok().as_deref(),
            Some("0")
        );
        assert!(latest_ui_assistant_text(&app).contains("resumed for this runtime"));
    }

    #[tokio::test]
    async fn slash_agents_doctor_uses_native_queue_audit() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let jobs = tmp.path().join("background_jobs");
        std::fs::create_dir_all(&jobs).expect("jobs dir");
        std::fs::write(
            jobs.join("one.json"),
            r#"{"id":"dupe","status":"running","task":"inspect"}"#,
        )
        .expect("write one");
        std::fs::write(
            jobs.join("two.json"),
            r#"{"id":"dupe","status":"queued","task":"inspect again"}"#,
        )
        .expect("write two");
        std::fs::write(jobs.join("bad.json"), "{not json").expect("write bad");
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/agents", &["doctor"])
            .await
            .expect("agents doctor");
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Queue manifest audit (native)"));
        assert!(out.contains("json=3"));
        assert!(out.contains("malformed=1"));
        assert!(out.contains("duplicate_ids=1"));
        assert!(!out.contains("audit_background_queue.py"));
    }

    #[tokio::test]
    async fn promoted_sethome_command_sets_status_and_clears_marker() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_sethome_command(&mut app, &["alpha-room"]).expect("set home");
        assert!(latest_ui_assistant_text(&app).contains("Home marker updated."));
        let marker = load_home_session_marker().expect("home marker");
        assert_eq!(
            marker.get("home").and_then(|v| v.as_str()),
            Some("alpha-room")
        );

        handle_sethome_command(&mut app, &["status"]).expect("home status");
        assert!(latest_ui_assistant_text(&app).contains("Home marker file:"));

        handle_sethome_command(&mut app, &["clear"]).expect("home clear");
        assert!(latest_ui_assistant_text(&app).contains("Cleared home marker."));
        assert!(load_home_session_marker().is_none());
    }

    #[tokio::test]
    async fn promoted_paste_command_uses_test_clipboard_override() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        std::env::set_var("HERMES_TEST_CLIPBOARD_TEXT", "alpha clipboard payload");
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_paste_command(&mut app, &[]).expect("paste command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Clipboard captured:"));
        assert!(output.contains("alpha clipboard payload"));
    }

    #[tokio::test]
    async fn promoted_gquota_command_emits_provider_diagnostics() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_gquota_command(&mut app, &[]).await.expect("gquota");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Gemini quota/auth diagnostics"));
        assert!(output.contains("active provider:"));
    }

    #[tokio::test]
    async fn promoted_image_command_queues_and_consumes_hint() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result =
            handle_image_command(&mut app, &["/tmp/example-image.png"]).expect("image queue");
        assert_eq!(result, CommandResult::Handled);
        assert_eq!(app.pending_image_hint(), Some("/tmp/example-image.png"));
        assert!(latest_ui_assistant_text(&app).contains("Image hint queued"));

        let prepared = app.prepare_user_message("analyze the screenshot");
        assert!(prepared.starts_with("[IMAGE_HINT] path=/tmp/example-image.png"));
        assert!(app.pending_image_hint().is_none());

        let cleared = handle_image_command(&mut app, &["clear"]).expect("image clear");
        assert_eq!(cleared, CommandResult::Handled);
        assert!(latest_ui_assistant_text(&app).contains("Cleared pending image hint"));
    }

    #[tokio::test]
    async fn promoted_feedback_command_writes_feedback_log() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_feedback_command(&mut app, &["solid", "repro", "steps"])
            .expect("feedback write");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Feedback captured in"));

        let path = feedback_log_path();
        let raw = std::fs::read_to_string(&path).expect("read feedback log");
        assert!(raw.contains("\"note\":\"solid repro steps\""));
    }

    #[tokio::test]
    async fn promoted_debug_dump_command_writes_session_snapshot() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        app.messages.push(hermes_core::Message::user("hello"));
        let result = handle_debug_dump_command(&mut app, &[]).expect("debug dump");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Debug snapshot written."));

        let sessions_dir = app.state_root.join("sessions");
        let count = std::fs::read_dir(sessions_dir)
            .expect("sessions dir")
            .filter_map(|entry| entry.ok())
            .count();
        assert!(count > 0);
    }

    #[tokio::test]
    async fn promoted_plan_status_command_emits_queue_summary() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_plan_command(&mut app, &["status"]).expect("plan status");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Planner queue status"));
        assert!(output.contains("queued="));
    }

    #[tokio::test]
    async fn promoted_lsp_status_command_emits_index_details() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_lsp_command(&mut app, &["status"]).expect("lsp status");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("LSP/code-index status"));
        assert!(output.contains("code_index_enabled"));
    }

    #[tokio::test]
    async fn promoted_approve_and_deny_commands_operate_on_pairing_store() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let store = PairingStore::open_default();
        store
            .save(&[crate::pairing_store::PairedDevice {
                device_id: "device-01".to_string(),
                name: Some("Test device".to_string()),
                status: PairingStatus::Pending,
                created_at: chrono::Utc::now().to_rfc3339(),
                last_seen: None,
                shared_secret: None,
            }])
            .expect("seed pairing store");

        handle_approve_command(&mut app, &["device-01"]).expect("approve");
        assert!(latest_ui_assistant_text(&app).contains("Approved device 'device-01'"));

        handle_deny_command(&mut app, &["device-01"]).expect("deny");
        assert!(latest_ui_assistant_text(&app).contains("Revoked device 'device-01'"));
    }

    #[test]
    fn test_acp_history_to_messages_preserves_multimodal_user_content_marker() {
        let history = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "check this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAA"}}
            ]
        })];
        let messages = acp_history_to_messages(&history, "");
        assert_eq!(messages.len(), 1);
        let content = messages[0].content.as_deref().unwrap_or("");
        assert!(content.starts_with(ACP_MULTIMODAL_PREFIX));
    }

    #[test]
    fn test_acp_history_to_messages_flattens_assistant_parts_to_text() {
        let history = vec![serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "done"},
                {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}
            ]
        })];
        let messages = acp_history_to_messages(&history, "");
        assert_eq!(messages.len(), 1);
        let content = messages[0].content.as_deref().unwrap_or("");
        assert!(content.contains("done"));
        assert!(content.contains("Attached image"));
    }

    #[test]
    fn test_acp_events_from_agent_messages_pairs_tool_results_by_call_id() {
        let messages = vec![
            hermes_core::Message::assistant_with_tool_calls(
                Some("checking files".to_string()),
                vec![
                    hermes_core::ToolCall {
                        id: "tc-read".to_string(),
                        function: hermes_core::FunctionCall {
                            name: "read_file".to_string(),
                            arguments: r#"{"path":"/etc/hosts"}"#.to_string(),
                        },
                        extra_content: None,
                    },
                    hermes_core::ToolCall {
                        id: "tc-web".to_string(),
                        function: hermes_core::FunctionCall {
                            name: "web_search".to_string(),
                            arguments: r#"{"query":"rust acp"}"#.to_string(),
                        },
                        extra_content: None,
                    },
                ],
            ),
            hermes_core::Message::tool_result("tc-read", "127.0.0.1 localhost"),
            hermes_core::Message::tool_result("tc-web", r#"{"data":{"web":[]}}"#),
        ];

        let events = acp_events_from_agent_messages("session-1", &messages);

        assert_eq!(events.len(), 4);
        assert_eq!(events[0].tool_call_id.as_deref(), Some("tc-read"));
        assert_eq!(events[0].tool_name.as_deref(), Some("read_file"));
        assert_eq!(events[0].arguments.as_ref().unwrap()["path"], "/etc/hosts");
        assert_eq!(events[1].tool_call_id.as_deref(), Some("tc-web"));
        assert_eq!(events[2].tool_call_id.as_deref(), Some("tc-read"));
        assert_eq!(events[2].tool_name.as_deref(), Some("read_file"));
        assert_eq!(events[2].result.as_deref(), Some("127.0.0.1 localhost"));
        assert_eq!(events[3].tool_call_id.as_deref(), Some("tc-web"));
        assert_eq!(events[3].tool_name.as_deref(), Some("web_search"));
    }

    #[test]
    fn test_acp_events_from_agent_messages_emits_native_todo_plan() {
        let todo_result = r#"{"todos":[{"id":"inspect","content":"Inspect ACP","status":"completed"},{"id":"patch","content":"Patch renderer","status":"in_progress"}]}"#;
        let messages = vec![
            hermes_core::Message::assistant_with_tool_calls(
                None,
                vec![hermes_core::ToolCall {
                    id: "tc-todo".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "todo".to_string(),
                        arguments: r#"{"todos":[]}"#.to_string(),
                    },
                    extra_content: None,
                }],
            ),
            hermes_core::Message::tool_result("tc-todo", todo_result),
        ];

        let events = acp_events_from_agent_messages("session-1", &messages);

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, hermes_acp::AcpEventKind::ToolCallStart);
        assert_eq!(events[1].kind, hermes_acp::AcpEventKind::ToolCallComplete);
        assert_eq!(events[2].kind, hermes_acp::AcpEventKind::PlanUpdate);
        assert_eq!(events[2].session_update.as_deref(), Some("plan"));
        let entries = events[2].entries.as_ref().expect("plan entries");
        assert_eq!(entries[0].content, "Inspect ACP");
        assert_eq!(entries[0].status, "completed");
        assert_eq!(entries[1].content, "Patch renderer");
        assert_eq!(entries[1].status, "in_progress");
    }

    #[test]
    fn test_acp_stream_callbacks_route_reasoning_and_message_deltas() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let callbacks = acp_stream_callbacks("session-1", events.clone());

        callbacks.on_thinking.as_ref().unwrap()("actual reasoning");
        callbacks.on_stream_delta.as_ref().unwrap()("streamed answer");
        callbacks.on_thinking.as_ref().unwrap()("   ");
        callbacks.on_stream_delta.as_ref().unwrap()("");

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, hermes_acp::AcpEventKind::AgentThoughtChunk);
        assert_eq!(
            events[0].session_update.as_deref(),
            Some("agent_thought_chunk")
        );
        assert_eq!(events[0].text.as_deref(), Some("actual reasoning"));
        assert_eq!(events[1].kind, hermes_acp::AcpEventKind::MessageDelta);
        assert_eq!(events[1].text.as_deref(), Some("streamed answer"));
    }

    #[test]
    fn test_acp_events_from_agent_messages_uses_fallback_for_untracked_tool_result() {
        let mut result = hermes_core::Message::tool_result("tc-untracked", "ok");
        result.name = Some("terminal".to_string());

        let events = acp_events_from_agent_messages("session-1", &[result]);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_call_id.as_deref(), Some("tc-untracked"));
        assert_eq!(events[0].tool_name.as_deref(), Some("terminal"));
        assert_eq!(events[0].result.as_deref(), Some("ok"));
    }

    #[test]
    fn test_acp_usage_from_agent_usage_maps_top_level_agent_fields() {
        let usage = hermes_core::UsageStats {
            prompt_tokens: 123,
            completion_tokens: 45,
            total_tokens: 168,
            estimated_cost: Some(0.0123),
        };

        let acp_usage = acp_usage_from_agent_usage(&usage);

        assert_eq!(acp_usage.input_tokens, 123);
        assert_eq!(acp_usage.output_tokens, 45);
        assert_eq!(acp_usage.total_tokens, 168);
        assert_eq!(acp_usage.thought_tokens, None);
        assert_eq!(acp_usage.cached_read_tokens, None);
    }

    #[tokio::test]
    async fn usage_command_reports_actual_session_usage_without_estimated_cost() {
        let _guard = env_test_lock();
        hermes_core::credits::clear_last_nous_credits_state();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        app.apply_agent_result(hermes_core::AgentResult {
            messages: vec![
                hermes_core::Message::user("measure usage"),
                hermes_core::Message::assistant("measured"),
            ],
            finished_naturally: true,
            total_turns: 1,
            tool_errors: Vec::new(),
            usage: Some(hermes_core::UsageStats {
                prompt_tokens: 12,
                completion_tokens: 3,
                total_tokens: 15,
                estimated_cost: Some(0.0123),
            }),
            interrupted: false,
            session_cost_usd: Some(0.0123),
            session_started_hooks_fired: false,
        });

        let result = handle_slash_command(&mut app, "/usage", &[])
            .await
            .expect("usage command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Last response: 12 prompt / 3 completion / 15 total tokens"));
        assert!(output.contains("Actual session: 12 prompt / 3 completion / 15 total tokens"));
        assert!(!output.contains("Actual cost"));
        assert!(!output.contains("$0.0123"));
        hermes_core::credits::clear_last_nous_credits_state();
    }

    #[tokio::test]
    async fn usage_command_includes_last_nous_credits_state() {
        let _guard = env_test_lock();
        hermes_core::credits::clear_last_nous_credits_state();
        hermes_core::credits::capture_nous_credits_from_pairs([
            ("x-nous-credits-version", "1"),
            ("x-nous-credits-remaining-micros", "12000000"),
            ("x-nous-credits-remaining-usd", "12.00"),
            ("x-nous-credits-subscription-micros", "5000000"),
            ("x-nous-credits-subscription-usd", "5.00"),
            ("x-nous-credits-subscription-limit-micros", "10000000"),
            ("x-nous-credits-subscription-limit-usd", "10.00"),
            ("x-nous-credits-rollover-micros", "1000000"),
            ("x-nous-credits-purchased-micros", "7000000"),
            ("x-nous-credits-purchased-usd", "7.00"),
            ("x-nous-credits-denominator-kind", "subscription_cap"),
            ("x-nous-credits-paid-access", "true"),
        ])
        .expect("capture credits");
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/usage", &[])
            .await
            .expect("usage command");
        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Nous credits"));
        assert!(output.contains("Subscription: 50% remaining (50% used)"));
        assert!(output.contains("Total usable: 12.00"));
        hermes_core::credits::clear_last_nous_credits_state();
    }

    #[tokio::test]
    async fn billing_command_renders_billing_surface_when_logged_out() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _real_home_guard = EnvVarGuard::set("HOME", tmp.path());
        let _auth_file_guard = EnvVarGuard::set("HERMES_AUTH_FILE", tmp.path().join("auth.json"));
        let _nous_oauth_guard =
            EnvVarGuard::set("HERMES_NOUS_OAUTH_FILE", tmp.path().join("nous_oauth.json"));
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/billing", &[])
            .await
            .expect("billing command");

        assert_eq!(result, CommandResult::Handled);
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("Nous billing"));
        assert!(output.contains("Not logged into Nous Portal"));
        assert!(output.contains("Manage on portal:"));
    }

    #[test]
    fn test_acp_setup_browser_dependency_checks_forward_yes_flag() {
        let mut calls = Vec::new();
        let checks = acp_setup_browser_dependency_checks(true, |command| {
            calls.push(command.to_string());
            true
        })
        .expect("dependencies should pass");

        assert_eq!(calls, vec!["node", "agent-browser"]);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].dependency, "node");
        assert_eq!(checks[1].dependency, "browser");
        assert!(checks.iter().all(|check| check.available));
        assert!(checks.iter().all(|check| !check.interactive));
    }

    #[test]
    fn command_on_path_prefers_managed_node_before_path() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _path_guard = EnvVarGuard::set("PATH", "");
        write_test_executable(&tmp.path().join("node").join("bin").join("node"));

        assert!(command_on_path("node"));
    }

    #[test]
    fn whatsapp_bridge_start_command_prefers_managed_npx() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _path_guard = EnvVarGuard::set("PATH", "");
        let npx = tmp.path().join("node").join("bin").join("npx");
        write_test_executable(&npx);

        let command = whatsapp_bridge_start_command();

        assert_ne!(command, "npx hermes-whatsapp-bridge");
        assert!(command.contains("npx"));
        assert!(command.contains("hermes-whatsapp-bridge"));
    }

    #[test]
    fn test_acp_setup_browser_dependency_checks_stops_on_node_failure() {
        let mut calls = Vec::new();
        let err = acp_setup_browser_dependency_checks(false, |command| {
            calls.push(command.to_string());
            command != "node"
        })
        .expect_err("node failure should stop setup-browser");

        assert_eq!(calls, vec!["node"]);
        assert!(err.to_string().contains("node"));
    }

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

    #[test]
    fn p1_trigger_triage_escalates_high_severity_events() {
        let _guard = env_test_lock();
        std::env::set_var("HERMES_TRIGGER_TRIAGE_MODE", "strict");
        let assessment = evaluate_trigger_triage(
            "webhook",
            "critical outage with secret key leak and panic in runtime",
        );
        assert_eq!(assessment.decision, TriggerTriageDecision::Escalate);
        assert!(assessment.requires_approval);
        assert!(assessment.severity >= 7);
        std::env::remove_var("HERMES_TRIGGER_TRIAGE_MODE");
    }

    #[test]
    fn p2_trigger_triage_feedback_persists_bias_and_influences_scoring() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let baseline = evaluate_trigger_triage("webhook", "timeout error while polling");
        let feedback_assessment = evaluate_trigger_triage("webhook", "critical outage and panic");
        append_triage_learning_feedback(
            "webhook",
            "critical outage and panic",
            "critical",
            &feedback_assessment,
        )
        .expect("append triage feedback");
        let (bias, _) = triage_learning_bias("webhook", "timeout error while polling");
        assert!(bias > 0);
        let after = evaluate_trigger_triage("webhook", "timeout error while polling");
        assert!(after.severity >= baseline.severity);
        assert!(
            trigger_triage_learning_state_path().exists(),
            "triage learning state file should be persisted"
        );
    }

    #[tokio::test]
    async fn p2_subconscious_profile_dry_run_blocks_high_risk_tasks() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let now = chrono::Utc::now().to_rfc3339();
        let state = SubconsciousQueueState {
            tasks: vec![SubconsciousTask {
                id: "sc-risky".to_string(),
                source: "test".to_string(),
                prompt: "rotate key and deploy to prod".to_string(),
                score: 4.2,
                risk: "high".to_string(),
                requires_approval: false,
                status: "pending".to_string(),
                job_id: None,
                created_at: now.clone(),
                updated_at: now,
            }],
        };
        save_subconscious_state(&state).expect("save subconscious state");

        handle_slash_command(
            &mut app,
            "/subconscious",
            &["run", "1", "--dry-run", "profile=strict"],
        )
        .await
        .expect("subconscious dry-run");
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Dry-run subconscious run profile=strict"));
        assert!(out.contains("blocked=1"));
    }

    #[tokio::test]
    async fn p2_walkthrough_insights_persists_events() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/walkthrough", &["start", "quick"])
            .await
            .expect("walkthrough start");
        handle_slash_command(&mut app, "/walkthrough", &["done", "boot-gate"])
            .await
            .expect("walkthrough done");
        handle_slash_command(&mut app, "/walkthrough", &["insights"])
            .await
            .expect("walkthrough insights");
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Walkthrough insights"));
        assert!(out.contains("resume_hint:"));
        assert!(walkthrough_events_path().exists());
    }

    #[tokio::test]
    async fn p2_integrations_snapshot_and_repair_commands_work() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/integrations", &["snapshot"])
            .await
            .expect("integrations snapshot");
        let snapshot_out = latest_ui_assistant_text(&app);
        assert!(snapshot_out.contains("Integration snapshot exported"));

        handle_slash_command(&mut app, "/integrations", &["repair"])
            .await
            .expect("integrations repair");
        let repair_out = latest_ui_assistant_text(&app);
        assert!(repair_out.contains("Integrations repair plan"));
    }

    #[tokio::test]
    async fn p2_compress_rules_autotune_apply_updates_runtime_env() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        std::env::remove_var("HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS");

        handle_slash_command(
            &mut app,
            "/compress",
            &["rules", "autotune", "apply", "user"],
        )
        .await
        .expect("compress autotune apply");
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Autotune applied"));
        assert!(
            std::env::var("HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS")
                .ok()
                .is_some(),
            "autotune should write runtime compression env"
        );
    }

    #[test]
    fn p2_oauth_runtime_gate_manifest_override_is_honored() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let manifest = tmp.path().join("oauth-manifest.json");
        std::fs::write(
            &manifest,
            r#"{
  "default_min_version": "99.0.0",
  "required_oauth_provider_ids": ["nous"],
  "provider_min_versions": { "nous": "99.0.0" }
}"#,
        )
        .expect("write manifest");
        std::env::set_var("HERMES_OAUTH_GATE_MANIFEST_PATH", &manifest);
        let (ok, detail) = oauth_runtime_gate_for_provider("nous").expect("oauth gate");
        assert!(!ok);
        assert!(detail.contains("required>=99.0.0"));
        assert!(detail.contains("oauth-manifest.json"));
        std::env::remove_var("HERMES_OAUTH_GATE_MANIFEST_PATH");
    }

    #[test]
    fn test_debug_alias_maps_to_debug_dump() {
        assert_eq!(canonical_command("/debug"), "/debug-dump");
    }

    #[test]
    fn test_upstream_compat_aliases_are_mapped() {
        assert_eq!(canonical_command("/topic"), "/title");
        assert_eq!(canonical_command("/reload-skills"), "/reload-skills");
        assert_eq!(canonical_command("/reload_skills"), "/reload-skills");
        assert_eq!(canonical_command("/swarms"), "/swarm");
        assert_eq!(canonical_command("/summary"), "/recap");
        assert_eq!(canonical_command("/whoami"), "/profile");
        assert_eq!(canonical_command("/v"), "/version");
        assert_eq!(canonical_command("/billing"), "/billing");
        assert_eq!(canonical_command("/credits"), "/usage");
        assert_eq!(canonical_command("/suggest"), "/suggestions");
        assert_eq!(canonical_command("/footer"), "/statusbar");
        assert_eq!(canonical_command("/indicator"), "/statusbar");
        assert_eq!(canonical_command("/tasks"), "/kanban");
        assert_eq!(canonical_command("/kanban"), "/kanban");
        assert_eq!(canonical_command("/busy"), "/status");
        assert_eq!(canonical_command("/bg"), "/background");
        assert_eq!(canonical_command("/curator"), "/skills");
        assert_eq!(canonical_command("/tt"), "/timetravel");
        assert_eq!(canonical_command("/rb"), "/runbook");
    }

    #[test]
    fn p3_swarm_commands_registered_and_completable() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/swarm"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/swarms"));
        assert!(autocomplete("/swa").contains(&"/swarm"));
    }

    #[tokio::test]
    async fn p3_swarm_status_plan_run_cancel_surface_is_handled() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        handle_slash_command(&mut app, "/swarm", &["status"])
            .await
            .expect("swarm status");
        let status = latest_ui_assistant_text(&app);
        assert!(status.contains("Swarm runtime"));

        handle_slash_command(&mut app, "/swarm", &["plan", "graph"])
            .await
            .expect("swarm plan");
        let plan = latest_ui_assistant_text(&app);
        assert!(plan.contains("Swarm execution plan"));
        assert!(plan.contains("\"mode\": \"graph\""));

        handle_slash_command(&mut app, "/swarm", &["on"])
            .await
            .expect("swarm on");
        handle_slash_command(&mut app, "/swarm", &["run", "4", "sequential"])
            .await
            .expect("swarm run");
        assert!(app.quorum_armed_once, "swarm run should arm quorum fanout");
        let run_msg = latest_ui_assistant_text(&app);
        assert!(run_msg.contains("Swarm run armed."));
        assert!(run_msg.contains("mode=sequential"));

        handle_slash_command(&mut app, "/swarm", &["cancel"])
            .await
            .expect("swarm cancel");
        assert!(!app.quorum_armed_once, "cancel should disarm run");
    }

    #[test]
    fn repo_review_budget_profile_application_sets_expected_env() {
        let _guard = env_test_lock();
        apply_repo_review_budget_profile(RepoReviewBudgetProfile::Aggressive);
        let runtime = RepoReviewBudgetRuntime::from_env();
        assert_eq!(runtime.profile, RepoReviewBudgetProfile::Aggressive);
        assert_eq!(runtime.repeat_threshold, 1);
        assert_eq!(runtime.low_signal_threshold, 1);
        assert_eq!(runtime.keep_repeat, 1);
        assert_eq!(runtime.keep_low_signal, 1);
        assert!(runtime.min_signal_score >= 0.34);

        apply_repo_review_budget_profile(RepoReviewBudgetProfile::Balanced);
        let runtime_balanced = RepoReviewBudgetRuntime::from_env();
        assert_eq!(runtime_balanced.profile, RepoReviewBudgetProfile::Balanced);
        assert_eq!(runtime_balanced.repeat_threshold, 2);
        assert_eq!(runtime_balanced.low_signal_threshold, 2);
    }

    #[test]
    fn task_depth_profile_application_sets_expected_env() {
        let _guard = env_test_lock();
        apply_task_depth_profile(TaskDepthProfile::Max);
        assert_eq!(
            std::env::var("HERMES_TASK_DEPTH_PROFILE").ok().as_deref(),
            Some("max")
        );
        assert_eq!(
            std::env::var("HERMES_MAX_ITERATIONS").ok().as_deref(),
            Some("250")
        );
        assert_eq!(
            std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE")
                .ok()
                .as_deref(),
            Some("off")
        );

        apply_task_depth_profile(TaskDepthProfile::Balanced);
        assert_eq!(
            std::env::var("HERMES_TASK_DEPTH_PROFILE").ok().as_deref(),
            Some("balanced")
        );
        assert_eq!(
            std::env::var("HERMES_MAX_ITERATIONS").ok().as_deref(),
            Some("50")
        );
    }

    #[test]
    fn test_recap_and_context_commands_are_registered() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/recap"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/context"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/auth"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/telemetry"));
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/runbook"));
        let recap = autocomplete("/rec");
        assert!(recap.contains(&"/recap"));
        let context = autocomplete("/cont");
        assert!(context.contains(&"/context"));
        let auth = autocomplete("/au");
        assert!(auth.contains(&"/auth"));
        let telemetry = autocomplete("/tele");
        assert!(telemetry.contains(&"/telemetry"));
        let runbook = autocomplete("/runb");
        assert!(runbook.contains(&"/runbook"));
    }

    #[test]
    fn test_memory_command_is_registered_completable_and_cataloged() {
        assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/memory"));
        let results = autocomplete("/mem");
        assert!(results.contains(&"/memory"));
        let catalog = render_command_catalog(Some("memory"));
        assert!(catalog.contains("/memory"));
        assert!(catalog.contains("Show memory backend status"));
    }

    #[test]
    fn test_render_memory_backend_status_reports_file_backend() {
        let tmp = tempdir().expect("tempdir");
        let memories = tmp.path().join("memories");
        std::fs::create_dir_all(&memories).expect("create memories dir");
        std::fs::write(memories.join("MEMORY.md"), "# Memory\nfact\n").expect("write memory");
        std::fs::write(memories.join("USER.md"), "# User\npreference\n").expect("write user");

        let status = render_memory_backend_status(tmp.path());
        assert!(status.contains("Memory provider: files (MEMORY.md + USER.md)"));
        assert!(status.contains("MEMORY.md"));
        assert!(status.contains("USER.md"));
    }

    #[test]
    fn test_render_mcp_runtime_status_includes_json_only_servers() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("mcp_servers.json");
        std::fs::write(
            &path,
            r#"{"contextlattice":{"url":"http://127.0.0.1:8075/mcp","enabled":true,"supports_parallel_tool_calls":true}}"#,
        )
        .expect("write mcp json");
        let cfg = crate::mcp_config::load_mcp_config(&path).expect("load mcp config");

        let status = render_mcp_runtime_status(&[], Some(&cfg), &path);
        assert!(status.contains("MCP runtime status"));
        assert!(status.contains("contextlattice"));
        assert!(status.contains("source:mcp_servers.json"));
        assert!(status.contains("json_only=[contextlattice]"));
    }

    #[test]
    fn test_render_mcp_runtime_status_reports_drift_between_yaml_and_json() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("mcp_servers.json");
        let cfg = crate::mcp_config::parse_mcp_config_json(
            r#"{"json-only":{"url":"https://example.com/mcp"}}"#,
        )
        .expect("parse mcp config");
        let yaml = vec![hermes_config::McpServerEntry {
            name: "yaml-only".to_string(),
            command: Some("local-mcp".to_string()),
            url: None,
            supports_parallel_tool_calls: false,
            keepalive_interval: None,
        }];

        let status = render_mcp_runtime_status(&yaml, Some(&cfg), &path);
        assert!(status.contains("yaml-only"));
        assert!(status.contains("json-only"));
        assert!(status.contains("config_only=[yaml-only]"));
        assert!(status.contains("json_only=[json-only]"));
    }

    #[tokio::test]
    async fn guard_provider_model_selection_soft_accepts_unlisted_codex_models() {
        let _guard = env_test_lock();
        std::env::set_var("HERMES_MODEL_CATALOG_GUARD", "1");
        let (guarded, note) = guard_provider_model_selection_for_config(
            "openai-codex:gpt-9-codex-preview",
            &GatewayConfig::default(),
        )
        .await
        .expect("codex soft-accept");
        assert_eq!(guarded, "openai-codex:gpt-9-codex-preview");
        assert!(note
            .as_deref()
            .unwrap_or_default()
            .contains("soft-accepted"));
        std::env::remove_var("HERMES_MODEL_CATALOG_GUARD");
    }

    #[test]
    fn alpha_loop_defaults_are_written_and_loadable() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        let path = crate::alpha_runtime::write_default_alpha_loops(true).expect("write defaults");
        assert!(path.exists());
        let loops = crate::alpha_runtime::load_alpha_loops().expect("load defaults");
        assert_eq!(loops.len(), 3);
        assert!(loops.iter().any(|l| l.id == "primary-objective-loop"));
        assert!(loops.iter().all(|l| !l.trading_sensitive));
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn test_autocomplete_includes_evolve() {
        let results = autocomplete("/evo");
        assert!(results.contains(&"/evolve"));
    }

    #[test]
    fn summarize_self_evolution_report_formats_fields() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("self-evolution-loop-test.json");
        std::fs::write(
            &path,
            r#"{
  "ok": false,
  "generated_at": "2026-05-02T00:00:00Z",
  "summary": { "intelligence_index": 66.67 },
  "recommendations": [{"id":"PARITY_DRIFT"}]
}"#,
        )
        .expect("write report");
        let line = summarize_self_evolution_report(&path, "self_evolution").expect("summary");
        assert!(line.contains("self_evolution=fail"));
        assert!(line.contains("idx=66.67"));
        assert!(line.contains("recs=1"));
    }

    #[test]
    fn self_evolution_recommendations_extracts_lines() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("self-evolution-loop-test.json");
        std::fs::write(
            &path,
            r#"{
  "recommendations": [
    {
      "id": "EVAL_REGRESSION",
      "severity": "P0",
      "title": "Recover eval trend before promotion",
      "command": "/ops eval run"
    }
  ]
}"#,
        )
        .expect("write report");
        let lines = self_evolution_recommendations(&path);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("EVAL_REGRESSION"));
        assert!(lines[0].contains("/ops eval run"));
    }

    #[test]
    fn native_session_eval_harness_writes_compatible_report() {
        let repo = tempdir().expect("repo");
        let home = tempdir().expect("home");
        let sessions_dir = home.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        std::fs::write(
            sessions_dir.join("rich-session.json"),
            r#"{
  "messages": [
    {"role":"user","content":"please inspect /objective status"},
    {"role":"assistant","content":"I will use tool_call evidence and apply_patch. [objective_patch] exists_now=true"},
    {"role":"user","content":"verify"},
    {"role":"assistant","content":"verified_exists=true"}
  ]
}"#,
        )
        .expect("write session");

        let (report, path) = run_session_eval_harness_native(repo.path(), &sessions_dir, 25, None)
            .expect("run native session eval");
        assert!(path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .starts_with("session-eval-harness-"));
        assert!(report["ok"].as_bool().expect("ok bool"));
        assert_eq!(report["summary"]["sessions_analyzed"], 1);
        assert_eq!(report["summary"]["tool_activity_sessions"], 1);
        assert_eq!(report["summary"]["objective_activity_sessions"], 1);
        assert_eq!(report["summary"]["patch_evidence_sessions"], 1);
        assert_eq!(report["summary"]["user_turns"], 2);
        assert_eq!(report["summary"]["assistant_turns"], 2);
        let on_disk = read_json_file(&path).expect("report on disk");
        assert_eq!(on_disk["summary"]["sessions_analyzed"], 1);
    }

    #[test]
    fn native_eval_trend_gate_matches_python_contract() {
        let repo = tempdir().expect("repo");
        let evals = repo.path().join("evals");
        std::fs::create_dir_all(&evals).expect("evals dir");
        let baseline = evals.join("baseline.json");
        let current = evals.join("current.json");
        std::fs::write(
            &baseline,
            r#"{"metrics":{"total":2,"pass_at_1":0.90,"total_duration":{"secs":20,"nanos":0},"total_cost_usd":1.0}}"#,
        )
        .expect("write baseline");
        std::fs::write(
            &current,
            r#"{"metrics":{"total":2,"pass_at_1":0.88,"total_duration":{"secs":22,"nanos":0},"total_cost_usd":1.1}}"#,
        )
        .expect("write current");

        let (report, path) = run_eval_trend_gate_native(
            repo.path(),
            Some(&current),
            Some(&baseline),
            None,
            EvalTrendGateOptions::default(),
        )
        .expect("run trend gate");
        assert!(report["ok"].as_bool().expect("ok bool"));
        assert_eq!(
            report["current_path"].as_str(),
            Some(current.to_string_lossy().as_ref())
        );
        assert_eq!(
            report["baseline_path"].as_str(),
            Some(baseline.to_string_lossy().as_ref())
        );
        assert_eq!(report["checks"][0]["name"], "pass_at_1_drop");
        assert!(report["checks"][0]["ok"].as_bool().expect("check ok bool"));
        assert!(path.exists());
    }

    #[test]
    fn native_eval_trend_gate_allows_missing_baseline_when_requested() {
        let repo = tempdir().expect("repo");
        let (report, path) = run_eval_trend_gate_native(
            repo.path(),
            None,
            None,
            None,
            EvalTrendGateOptions {
                allow_missing_baseline: true,
                ..Default::default()
            },
        )
        .expect("run missing-input gate");
        assert!(report["ok"].as_bool().expect("ok bool"));
        assert_eq!(report["reason"], "missing_eval_inputs");
        assert!(path.exists());
    }

    #[tokio::test]
    async fn ops_eval_run_uses_native_report_not_python_script() {
        let _guard = env_test_lock();
        let repo = tempdir().expect("repo");
        let home = tempdir().expect("home");
        let _home_guard = TempHomeGuard::new(home.path());
        let previous_repo_root = std::env::var("HERMES_REPO_ROOT").ok();
        std::env::set_var("HERMES_REPO_ROOT", repo.path());
        let sessions_dir = home.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        std::fs::write(
            sessions_dir.join("rich-session.json"),
            r#"{"messages":[
  {"role":"user","content":"run /objective status"},
  {"role":"assistant","content":"tool_call result with apply_patch exists_now=true"},
  {"role":"user","content":"ok"},
  {"role":"assistant","content":"done"}
]}"#,
        )
        .expect("write session");
        let mut app = build_test_app_with_stream(home.path()).await;

        handle_ops_eval_command(&mut app, &["run"])
            .await
            .expect("handle ops eval run");
        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("\"sessions_analyzed\": 1"));
        assert!(out.contains("session-eval-harness-"));
        assert!(!out.contains("python3 scripts/run-session-eval-harness.py"));

        match previous_repo_root {
            Some(value) => std::env::set_var("HERMES_REPO_ROOT", value),
            None => std::env::remove_var("HERMES_REPO_ROOT"),
        }
    }

    #[test]
    fn summarize_performance_autopilot_report_formats_fields() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("performance-autopilot-test.json");
        std::fs::write(
            &path,
            r#"{
  "ok": true,
  "generated_at": "2026-05-08T00:00:00Z",
  "recommendations": [
    {"id":"PERF_STABLE", "severity":"P3", "title":"stable", "recommendation":"none"}
  ]
}"#,
        )
        .expect("write report");
        let line = summarize_performance_autopilot_report(&path, "autopilot").expect("summary");
        assert!(line.contains("autopilot=pass"));
        assert!(line.contains("recs=1"));
        assert!(line.contains("severe=0"));
    }

    #[test]
    fn performance_autopilot_recommendations_extract_lines() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("performance-autopilot-test.json");
        std::fs::write(
            &path,
            r#"{
  "recommendations": [
    {
      "id":"HOTPATH_SLOW",
      "severity":"P1",
      "title":"Tool policy hot-path latency above target",
      "recommendation":"Keep HERMES_TOOL_POLICY_PRESET=standard"
    }
  ]
}"#,
        )
        .expect("write report");
        let recs = performance_autopilot_recommendations(&path);
        assert_eq!(recs.len(), 1);
        assert!(recs[0].contains("HOTPATH_SLOW"));
        assert!(recs[0].contains("recommendation"));
    }

    #[test]
    fn native_performance_autopilot_recommends_throughput_for_slow_hotpath() {
        let hotpath = serde_json::json!({
            "ok": true,
            "stdout_tail": "tool_policy_hot_path_ns_per_eval=13000\n",
            "stderr_tail": "",
            "exit_code": 0,
        });
        let pass = serde_json::json!({
            "ok": true,
            "stdout_tail": "{}",
            "stderr_tail": "",
            "exit_code": 0,
        });
        let context = serde_json::json!({
            "ok": true,
            "stdout_tail": r#"{"health":{"ok":true},"warnings":[],"context_pack":{"retrieval":{"source_counts":{"qdrant":2},"fallback_counts":{"python_hot_path_total":0}}},"status":{"queue":{"pendingTotal":0}}}"#,
            "stderr_tail": "",
            "exit_code": 0,
        });

        let recs = build_performance_autopilot_recommendations(&hotpath, &pass, &pass, &context);
        assert!(recs
            .iter()
            .any(|rec| rec.get("id").and_then(|v| v.as_str()) == Some("HOTPATH_SLOW")));
        let adaptive =
            compute_performance_autopilot_indexes(&hotpath, &pass, &pass, &context, &recs);
        assert_eq!(adaptive["profile_recommendation"], "throughput");
        assert!(adaptive["adaptive_index"].as_f64().unwrap_or(0.0) > 80.0);
    }

    #[test]
    fn native_performance_autopilot_env_writer_uses_safe_actions() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("autopilot.env");
        let report = serde_json::json!({
            "generated_at": "2026-06-04T00:00:00Z",
            "profile_recommendation": "throughput",
            "recommendations": [
                {"id":"HOTPATH_SLOW","severity":"P1","title":"slow","recommendation":"tune"}
            ],
            "adaptive_actions": [
                {"key":"HERMES_PERF_AUTOPILOT_PROFILE","value":"throughput","reason":"profile"},
                {"key":"HERMES_MODEL_CATALOG_GUARD","value":"1","reason":"guard"}
            ]
        });

        write_performance_autopilot_env(&path, &report).expect("write env");
        let raw = std::fs::read_to_string(&path).expect("read env");
        assert!(raw.contains("HERMES_TOOL_POLICY_PRESET=standard"));
        assert!(raw.contains("HERMES_MODEL_CATALOG_GUARD=1"));
        assert!(raw.contains("HERMES_PERF_AUTOPILOT_PROFILE=throughput"));
        let kvs = parse_env_file_kv(&path);
        assert!(kvs
            .iter()
            .any(|(k, v)| k == "HERMES_MODEL_CATALOG_GUARD" && v == "1"));
    }

    #[tokio::test]
    async fn native_slo_auto_rollback_runs_rollback_on_violation() {
        let repo = tempdir().expect("repo");
        let rollback_marker = repo.path().join("rollback.marker");
        let rollback_cmd = format!("printf rolled-back > {}", rollback_marker.display());
        let (report, path) =
            run_slo_auto_rollback_native(repo.path(), "false", &rollback_cmd, false, None)
                .await
                .expect("run slo");
        assert!(!report["ok"].as_bool().expect("ok bool"));
        assert!(report["violated"].as_bool().expect("violated bool"));
        assert!(report["rollback"]["ok"].as_bool().expect("rollback ok bool"));
        assert!(path.exists());
        assert_eq!(
            std::fs::read_to_string(&rollback_marker).expect("read marker"),
            "rolled-back"
        );
    }

    #[test]
    fn native_self_evolution_recommendations_use_runtime_commands() {
        let sections = serde_json::json!({
            "golden_parity": {"ok": true},
            "eval_trend": {"ok": false},
            "elite_sync": {"ok": false}
        });
        let recs = build_self_evolution_recommendations_native("ship rust surfaces", &sections);
        assert!(recs
            .iter()
            .any(|rec| rec.get("id").and_then(|v| v.as_str()) == Some("EVAL_REGRESSION")));
        assert!(recs
            .iter()
            .any(|rec| rec.get("command").and_then(|v| v.as_str()) == Some("/ops gate elite")));
        assert!(recs.iter().all(|rec| !rec
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("python3")));
    }

    #[test]
    fn native_parity_sections_read_release_and_backlog_gates() {
        let repo = tempdir().expect("repo");
        let parity_dir = repo.path().join("docs/parity");
        std::fs::create_dir_all(&parity_dir).expect("parity dir");
        std::fs::write(
            parity_dir.join("global-parity-proof.json"),
            r#"{"release_gate":{"pass":true}}"#,
        )
        .expect("write proof");
        std::fs::write(
            parity_dir.join("shared-diff-backlog.json"),
            r#"{"summary":{"pending_classification":0,"pending_review":0}}"#,
        )
        .expect("write backlog");

        assert!(parity_release_gate_section(repo.path())["ok"]
            .as_bool()
            .expect("release gate ok bool"));
        assert!(shared_backlog_gate_section(repo.path())["ok"]
            .as_bool()
            .expect("backlog gate ok bool"));
    }

    #[test]
    fn parse_env_file_kv_ignores_comments() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("autopilot.env");
        std::fs::write(
            &path,
            "# comment\nHERMES_TOOL_POLICY_PRESET=standard\n \nINVALID_LINE\nHERMES_REPLAY_ENABLED=1\n",
        )
        .expect("write env");
        let kvs = parse_env_file_kv(&path);
        assert_eq!(kvs.len(), 2);
        assert_eq!(kvs[0].0, "HERMES_TOOL_POLICY_PRESET");
        assert_eq!(kvs[1].0, "HERMES_REPLAY_ENABLED");
    }

    #[test]
    fn test_autocomplete_includes_autopilot() {
        let results = autocomplete("/auto");
        assert!(results.contains(&"/autopilot"));
    }

    #[test]
    fn canonical_command_maps_pilot_alias() {
        assert_eq!(canonical_command("/pilot"), "/autopilot");
    }

    #[test]
    fn test_autocomplete_includes_raw_controls() {
        let results = autocomplete("/ra");
        assert!(results.contains(&"/raw"));
    }

    #[test]
    fn test_autocomplete_ops_control_plane() {
        let results = autocomplete("/op");
        assert!(results.contains(&"/ops"));
    }

    #[test]
    fn test_autocomplete_fuzzy_prefers_close_matches() {
        let results = autocomplete("/mdl");
        assert!(!results.is_empty());
        assert_eq!(results[0], "/model");
    }

    #[test]
    fn test_autocomplete_matches_description_terms() {
        let results = autocomplete("/quota");
        assert!(results.contains(&"/gquota"));
    }

    #[test]
    fn test_autocomplete_exact() {
        let results = autocomplete("/help");
        assert!(!results.is_empty());
        assert_eq!(results[0], "/help");
    }

    #[test]
    fn test_autocomplete_no_match() {
        let results = autocomplete("/xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn test_help_for_known_command() {
        assert!(help_for("/help").is_some());
        assert!(help_for("/model").is_some());
    }

    #[test]
    fn test_help_for_unknown_command() {
        assert!(help_for("/unknown").is_none());
    }

    #[test]
    fn test_command_result_equality() {
        assert_eq!(CommandResult::Handled, CommandResult::Handled);
        assert_ne!(CommandResult::Handled, CommandResult::Quit);
    }

    #[tokio::test]
    async fn test_mcp_sentrux_setup_syncs_json_and_yaml() {
        let tmp = tempdir().expect("tempdir");
        let config_dir = tmp.path().join("hermes-home");
        std::fs::create_dir_all(&config_dir).expect("create config dir");

        upsert_sentrux_mcp_profile(&config_dir).expect("sentrux setup helper");

        let mcp_json = config_dir.join("mcp_servers.json");
        assert!(mcp_json.exists(), "mcp_servers.json should be created");
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&mcp_json).expect("read mcp_servers.json"),
        )
        .expect("parse mcp json");
        let sentrux = json
            .get(SENTRUX_MCP_SERVER_NAME)
            .expect("sentrux entry should exist");
        assert_eq!(
            sentrux.get("command").and_then(|v| v.as_str()),
            Some(SENTRUX_MCP_COMMAND)
        );
        assert_eq!(
            sentrux
                .get("args")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str()),
            Some(SENTRUX_MCP_ARG)
        );
        assert!(
            sentrux
                .get("supports_parallel_tool_calls")
                .and_then(|v| v.as_bool())
                .expect("sentrux parallel flag")
        );

        let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
            .expect("load config.yaml");
        assert!(
            cfg.mcp_servers
                .iter()
                .any(|entry| entry.name == SENTRUX_MCP_SERVER_NAME
                    && entry.command.as_deref() == Some("sentrux --mcp")
                    && entry.supports_parallel_tool_calls),
            "config.yaml mcp_servers should include sentrux command"
        );
    }

    #[tokio::test]
    async fn test_mcp_sentrux_remove_syncs_json_and_yaml() {
        let tmp = tempdir().expect("tempdir");
        let config_dir = tmp.path().join("hermes-home");
        std::fs::create_dir_all(&config_dir).expect("create config dir");

        upsert_sentrux_mcp_profile(&config_dir).expect("sentrux setup helper");
        remove_sentrux_mcp_profile(&config_dir).expect("sentrux remove helper");

        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(config_dir.join("mcp_servers.json")).expect("read mcp json"),
        )
        .expect("parse mcp json");
        assert!(
            json.get(SENTRUX_MCP_SERVER_NAME).is_none(),
            "mcp_servers.json should remove sentrux"
        );

        let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
            .expect("load config.yaml");
        assert!(
            cfg.mcp_servers
                .iter()
                .all(|entry| entry.name != SENTRUX_MCP_SERVER_NAME),
            "config.yaml mcp_servers should remove sentrux"
        );
    }

    #[tokio::test]
    async fn test_mcp_unreal_engine_setup_syncs_json_and_yaml() {
        let tmp = tempdir().expect("tempdir");
        let config_dir = tmp.path().join("hermes-home");
        std::fs::create_dir_all(&config_dir).expect("create config dir");

        upsert_unreal_mcp_profile(&config_dir).expect("unreal setup helper");

        let mcp_json = config_dir.join("mcp_servers.json");
        assert!(mcp_json.exists(), "mcp_servers.json should be created");
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&mcp_json).expect("read mcp_servers.json"),
        )
        .expect("parse mcp json");
        let unreal = json
            .get(UNREAL_MCP_SERVER_NAME)
            .expect("unreal entry should exist");
        assert_eq!(
            unreal.get("url").and_then(|v| v.as_str()),
            Some(UNREAL_MCP_URL)
        );
        assert!(
            !unreal
                .get("supports_parallel_tool_calls")
                .and_then(|v| v.as_bool())
                .expect("unreal parallel flag")
        );
        assert_eq!(
            unreal.get("keepalive_interval").and_then(|v| v.as_u64()),
            Some(10)
        );

        let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
            .expect("load config.yaml");
        assert!(
            cfg.mcp_servers
                .iter()
                .any(|entry| entry.name == UNREAL_MCP_SERVER_NAME
                    && entry.url.as_deref() == Some(UNREAL_MCP_URL)
                    && !entry.supports_parallel_tool_calls
                    && entry.keepalive_interval == Some(10)),
            "config.yaml mcp_servers should include the Unreal HTTP profile"
        );

        let (json_present, yaml_present) = unreal_mcp_status(&config_dir);
        assert!(json_present);
        assert!(yaml_present);
    }

    #[tokio::test]
    async fn test_mcp_unreal_engine_remove_syncs_json_and_yaml() {
        let tmp = tempdir().expect("tempdir");
        let config_dir = tmp.path().join("hermes-home");
        std::fs::create_dir_all(&config_dir).expect("create config dir");

        upsert_unreal_mcp_profile(&config_dir).expect("unreal setup helper");
        remove_unreal_mcp_profile(&config_dir).expect("unreal remove helper");

        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(config_dir.join("mcp_servers.json")).expect("read mcp json"),
        )
        .expect("parse mcp json");
        assert!(
            json.get(UNREAL_MCP_SERVER_NAME).is_none(),
            "mcp_servers.json should remove unreal-engine"
        );

        let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
            .expect("load config.yaml");
        assert!(
            cfg.mcp_servers
                .iter()
                .all(|entry| entry.name != UNREAL_MCP_SERVER_NAME),
            "config.yaml mcp_servers should remove unreal-engine"
        );
    }

    #[test]
    fn test_default_skill_tap_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/MiniMax-AI/cli::skill"));
    }

    #[test]
    fn test_autoresearch_default_skill_tap_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/github/awesome-copilot::skills"));
    }

    #[test]
    fn test_nous_official_default_skill_taps_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/NousResearch/hermes-agent::skills"));
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/NousResearch/hermes-agent::optional-skills"));
    }

    #[test]
    fn test_official_skill_path_candidates_cover_skills_and_optional() {
        let candidates = official_skill_path_candidates("creative/comfyui");
        assert_eq!(
            candidates,
            vec![
                "skills/creative/comfyui".to_string(),
                "optional-skills/creative/comfyui".to_string(),
            ]
        );

        let rooted = official_skill_path_candidates("optional-skills/security/1password");
        assert_eq!(
            rooted,
            vec!["optional-skills/security/1password".to_string()]
        );
    }

    #[test]
    fn test_mattpocock_default_skill_tap_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/mattpocock/skills::skills"));
    }

    #[test]
    fn test_merged_skill_taps_deduplicates_default() {
        let merged = merged_skill_taps(&["https://github.com/MiniMax-AI/cli::skill".to_string()]);
        assert_eq!(
            merged
                .iter()
                .filter(|tap| tap.as_str() == "https://github.com/MiniMax-AI/cli::skill")
                .count(),
            1
        );
    }

    #[test]
    fn parse_skill_tap_spec_parses_github_url_with_override() {
        let parsed =
            parse_skill_tap_spec("https://github.com/openai/skills::skills").expect("tap parse");
        assert_eq!(parsed.repo, "openai/skills");
        assert_eq!(parsed.path, "skills");
    }

    #[test]
    fn parse_skill_tap_spec_parses_tree_url() {
        let parsed = parse_skill_tap_spec("https://github.com/anthropics/skills/tree/main/skills")
            .expect("tap parse");
        assert_eq!(parsed.repo, "anthropics/skills");
        assert_eq!(parsed.path, "skills");
    }

    mod skill_model_policy;

}
