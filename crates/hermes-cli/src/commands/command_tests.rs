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

    fn write_fake_editor(path: &Path, body: &str, mode: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let script = if mode == "clear" {
            "#!/bin/sh\n: > \"$1\"\n".to_string()
        } else {
            format!("#!/bin/sh\ncat >> \"$1\" <<'EOF'\n{body}\nEOF\n")
        };
        std::fs::write(path, script).expect("write fake editor");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake editor");
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

        let saved_path = setup_memory_provider_target(
            "honcho",
            &MemorySetupCliOptions::yes_only(true),
        )
        .expect("setup honcho")
        .config_path;

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

        let path = setup_memory_provider_target(
            "openviking",
            &MemorySetupCliOptions::yes_only(true),
        )
        .expect("setup openviking")
        .config_path;

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
    fn memory_supermemory_setup_target_writes_runtime_config() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home = TempHomeGuard::new(tmp.path());
        let _api_key = EnvVarGuard::set("SUPERMEMORY_API_KEY", "sm-test-key");
        let _base_url = EnvVarGuard::set("SUPERMEMORY_BASE_URL", "https://api.supermemory.ai");
        let _container = EnvVarGuard::set("SUPERMEMORY_CONTAINER_TAG", "hermes-tests");

        let path = setup_memory_provider_target(
            "supermemory",
            &MemorySetupCliOptions::yes_only(true),
        )
        .expect("setup supermemory")
        .config_path;

        assert_eq!(path, tmp.path().join("supermemory.json"));
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("json");
        assert_eq!(parsed["api_key"], "sm-test-key");
        assert_eq!(parsed["container_tag"], "hermes-tests");
        assert!(parsed["auto_recall"].as_bool().expect("auto_recall"));
        assert!(parsed["auto_capture"].as_bool().expect("auto_capture"));

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
    fn memory_byterover_setup_target_writes_auto_extract_config() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home = TempHomeGuard::new(tmp.path());
        let _api_key = EnvVarGuard::set("BRV_API_KEY", "brv-test-key");

        let path = setup_memory_provider_target(
            "byterover",
            &MemorySetupCliOptions::yes_only(true),
        )
        .expect("setup byterover")
        .config_path;

        assert_eq!(path, tmp.path().join("byterover.json"));
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("json");
        assert_eq!(parsed["api_key"], "brv-test-key");
        assert!(parsed["auto_extract"].as_bool().expect("auto_extract"));
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
    async fn moa_without_prompt_is_usage_only() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let before_model = app.current_model.clone();

        let result = handle_slash_command(&mut app, "/moa", &[])
            .await
            .expect("moa usage command");

        assert_eq!(result, CommandResult::Handled);
        assert_eq!(app.current_model, before_model);
        let text = latest_ui_assistant_text(&app);
        assert!(text.contains("Usage: /moa <prompt>"));
        assert!(text.contains("Use /model to switch"));
    }

    #[tokio::test]
    async fn prompt_command_reads_editor_and_queues_agent_seed() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _visual_guard = EnvVarGuard::remove("VISUAL");
        let editor = tmp.path().join("fake-editor.sh");
        write_fake_editor(&editor, "rest of prompt\nUse tests.", "append");
        let _editor_guard = EnvVarGuard::set("EDITOR", editor.as_os_str());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/prompt", &["DRAFT:"])
            .await
            .expect("prompt command");

        assert_eq!(result, CommandResult::Handled);
        let seed = app.take_pending_agent_seed().expect("queued seed");
        assert!(seed.starts_with("DRAFT:"));
        assert!(seed.contains("rest of prompt"));
        assert!(seed.contains("Use tests."));
        assert!(!seed.contains("#!"));
        assert!(latest_ui_assistant_text(&app).contains("Prompt captured from editor"));
    }

    #[tokio::test]
    async fn prompt_command_empty_editor_buffer_cancels_seed() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let _visual_guard = EnvVarGuard::remove("VISUAL");
        let editor = tmp.path().join("fake-editor-clear.sh");
        write_fake_editor(&editor, "", "clear");
        let _editor_guard = EnvVarGuard::set("EDITOR", editor.as_os_str());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/compose", &[])
            .await
            .expect("prompt command");

        assert_eq!(result, CommandResult::Handled);
        assert!(app.take_pending_agent_seed().is_none());
        assert!(latest_ui_assistant_text(&app).contains("Empty prompt"));
    }

    #[tokio::test]
    async fn learn_command_records_learning_request_for_next_turn() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/learn", &["docs", "and", "chat"])
            .await
            .expect("learn command");

        assert_eq!(result, CommandResult::Handled);
        assert_eq!(app.pending_system_note_count(), 1);
        assert!(latest_ui_assistant_text(&app).contains("Learning request captured"));
    }

    #[tokio::test]
    async fn hatch_command_enables_pet_and_queues_design_brief() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;

        let result = handle_slash_command(&mut app, "/hatch", &["hyped", "otter", "navigator"])
            .await
            .expect("hatch command");

        assert_eq!(result, CommandResult::Handled);
        assert!(app.pet_settings().enabled);
        assert_eq!(app.pet_settings().species, "otter");
        assert_eq!(app.pet_settings().mood, "hyped");
        assert_eq!(app.pending_system_note_count(), 1);
        assert!(latest_ui_assistant_text(&app).contains("Pet hatch request captured"));
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

    include!("tests/promoted_commands.rs");
    include!("tests/surface_and_compression.rs");

    mod runtime_parity_ops;
    mod skill_model_policy;

}
