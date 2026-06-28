#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
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

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn test_hindsight_plugin_name() {
        let plugin = HindsightPlugin::new();
        assert_eq!(plugin.name(), "hindsight");
    }

    #[test]
    fn test_hindsight_tool_schemas() {
        let plugin = HindsightPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 3);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"hindsight_retain"));
        assert!(names.contains(&"hindsight_recall"));
        assert!(names.contains(&"hindsight_reflect"));
    }

    #[test]
    fn test_hindsight_context_mode_hides_tools() {
        let plugin = HindsightPlugin::new();
        *plugin.config.lock().unwrap() = Some(HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: "context".into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 1,
            retain_context: String::new(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        });
        assert!(plugin.get_tool_schemas().is_empty());
    }

    #[test]
    fn test_hindsight_system_prompt_modes() {
        let plugin = HindsightPlugin::new();
        let make_config = |mode: &str| HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: mode.into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 1,
            retain_context: String::new(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        };

        *plugin.config.lock().unwrap() = Some(make_config("hybrid"));
        assert!(plugin.system_prompt_block().contains("hindsight_recall"));

        *plugin.config.lock().unwrap() = Some(make_config("context"));
        assert!(plugin.system_prompt_block().contains("context mode"));

        *plugin.config.lock().unwrap() = Some(make_config("tools"));
        assert!(plugin.system_prompt_block().contains("tools mode"));
    }

    #[test]
    fn test_hindsight_handle_tool_missing_args() {
        let plugin = HindsightPlugin::new();
        let result = plugin.handle_tool_call("hindsight_recall", &json!({}));
        assert!(result.contains("error"));
    }

    #[test]
    fn test_hindsight_recall_body_defaults_to_observations() {
        let body = hindsight_recall_body("dark mode", "mid", 4096, &default_recall_types());
        assert_eq!(body["query"], "dark mode");
        assert_eq!(body["budget"], "mid");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["types"], json!(["observation"]));
    }

    #[test]
    fn test_hindsight_save_config_writes_owner_only() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let path = tmp.path().join("hindsight").join("config.json");

        HindsightPlugin::new()
            .save_config(&json!({"api_key":"hd-secret"}))
            .expect("save config");

        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("parse config");
        assert_eq!(parsed["api_key"], "hd-secret");

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
    fn test_parse_recall_types_accepts_string_or_list() {
        assert_eq!(
            parse_recall_types_value(&json!("observation,world, experience")),
            Some(vec![
                "observation".to_string(),
                "world".to_string(),
                "experience".to_string()
            ])
        );
        assert_eq!(
            parse_recall_types_value(&json!(["world", " experience ", "", 7])),
            Some(vec!["world".to_string(), "experience".to_string()])
        );
        assert_eq!(parse_recall_types_value(&json!(" , ")), None);
    }

    #[test]
    fn test_parse_retain_tags_and_observation_scopes() {
        assert_eq!(
            parse_retain_tags_value(&json!("project:ultra, session:s1, project:ultra")),
            Some(vec!["project:ultra".to_string(), "session:s1".to_string()])
        );
        assert_eq!(
            parse_retain_tags_value(&json!(["alpha", " beta ", "", "alpha"])),
            Some(vec!["alpha".to_string(), "beta".to_string()])
        );
        assert_eq!(
            merge_retain_tags(
                &["alpha".into(), "beta".into()],
                &["beta".into(), "gamma".into()]
            ),
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
        assert_eq!(
            parse_observation_scopes_value(&json!("per_tag")),
            Some(json!("per_tag"))
        );
        assert_eq!(
            parse_observation_scopes_value(&json!(["alpha", "beta"])),
            Some(json!([["alpha", "beta"]]))
        );
        assert_eq!(
            parse_observation_scopes_value(&json!("[[\"alpha\"],[\"alpha\",\"beta\"]]")),
            Some(json!([["alpha"], ["alpha", "beta"]]))
        );
        assert_eq!(parse_observation_scopes_value(&json!("invalid")), None);
    }

    #[test]
    fn test_resolve_bank_id_template_sanitizes_and_collapses() {
        let bank = resolve_bank_id_template(
            "hermes-{profile}-{user}-{session}",
            "hermes",
            &[
                ("profile", "dev/workspace".to_string()),
                ("workspace", String::new()),
                ("platform", String::new()),
                ("user", "u@id".to_string()),
                ("session", "sess_123".to_string()),
            ],
        );
        assert_eq!(bank, "hermes-dev-workspace-u-id-sess_123");
    }

    #[test]
    fn test_resolve_bank_id_template_unknown_placeholder_falls_back() {
        let bank = resolve_bank_id_template(
            "hermes-{unknown}",
            "fallback-bank",
            &[("profile", "p1".to_string())],
        );
        assert_eq!(bank, "fallback-bank");
    }

    fn write_hindsight_config(hermes_home: &std::path::Path, value: &Value) {
        let path = hermes_home.join("hindsight").join("config.json");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(value).expect("serialize config"),
        )
        .expect("write config");
    }

    #[test]
    fn test_config_accepts_snake_case_api_key_and_timeout_aliases() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");
        let _timeout = EnvGuard::remove("HINDSIGHT_TIMEOUT");
        let _retain_tags = EnvGuard::remove("HINDSIGHT_RETAIN_TAGS");
        let _observation_scopes = EnvGuard::remove("HINDSIGHT_RETAIN_OBSERVATION_SCOPES");

        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(
            tmp.path(),
            &json!({
                "mode": "cloud",
                "api_key": "snake-secret",
                "timeout": 42,
                "retain_tags": ["project:ultra", "session:s1", "project:ultra"],
                "observation_scopes": [["project:ultra"], ["project:ultra", "session:s1"]]
            }),
        );
        let cfg = HindsightConfig::load(tmp.path().to_str().expect("tmp path"));
        assert_eq!(cfg.api_key, "snake-secret");
        assert_eq!(cfg.timeout_secs, 42);
        assert_eq!(
            cfg.retain_tags,
            vec!["project:ultra".to_string(), "session:s1".to_string()]
        );
        assert_eq!(
            cfg.observation_scopes,
            Some(json!([["project:ultra"], ["project:ultra", "session:s1"]]))
        );

        write_hindsight_config(
            tmp.path(),
            &json!({"mode": "cloud", "apiKey": "camel-secret", "hindsight_timeout": 17}),
        );
        let cfg = HindsightConfig::load(tmp.path().to_str().expect("tmp path"));
        assert_eq!(cfg.api_key, "camel-secret");
        assert_eq!(cfg.timeout_secs, 17);
    }

    #[test]
    fn test_config_uses_env_timeout_and_normalizes_legacy_local_mode() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::set("HINDSIGHT_MODE", "local_embedded");
        let _timeout = EnvGuard::set("HINDSIGHT_TIMEOUT", "77");

        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(tmp.path(), &json!({}));

        let cfg = HindsightConfig::load(tmp.path().to_str().expect("tmp path"));
        assert_eq!(cfg.mode, "local_external");
        assert_eq!(cfg.api_url, DEFAULT_LOCAL_URL);
        assert_eq!(cfg.timeout_secs, 77);
    }

    #[test]
    fn test_available_with_local_external_config_mode() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _ultra_home = EnvGuard::remove("HERMES_AGENT_ULTRA_HOME");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");

        write_hindsight_config(tmp.path(), &json!({"mode": "local_external"}));

        assert!(HindsightPlugin::new().is_available());
    }

    #[test]
    fn test_turn_payload_preserves_non_ascii_and_document_id() {
        let turn =
            hindsight_turn_payload("Café 東京 🚀", "Zażółć gęślą jaźń", "2026-06-08T00:00:00Z");
        assert!(turn.contains("Café 東京 🚀"));
        assert!(turn.contains("Zażółć gęślą jaźń"));
        assert!(!turn.contains("\\u"));

        let parsed: Value = serde_json::from_str(&turn).expect("turn json");
        assert_eq!(parsed[0]["role"], "user");
        assert_eq!(parsed[0]["content"], "Café 東京 🚀");
        assert_eq!(parsed[1]["role"], "assistant");
        assert_eq!(parsed[1]["content"], "Zażółć gęślą jaźń");

        let content = format!("[{}]", turn);
        let body = hindsight_sync_turn_body(
            &content,
            "conversation",
            false,
            Some("session-1-doc"),
            Some("append"),
            &["project:ultra".to_string(), "session:session-1".to_string()],
            Some(&json!("per_tag")),
        );
        assert_eq!(body["async"], false);
        assert_eq!(body["document_id"], "session-1-doc");
        assert_eq!(body["items"][0]["update_mode"], "append");
        assert_eq!(body["items"][0]["content"], content);
        assert_eq!(body["items"][0]["context"], "conversation");
        assert_eq!(
            body["items"][0]["tags"],
            json!(["project:ultra", "session:session-1"])
        );
        assert_eq!(body["items"][0]["observation_scopes"], json!("per_tag"));
    }

    #[test]
    fn test_hindsight_version_probe_semver_gate() {
        assert!(hindsight_version_meets_minimum(Some("0.5.0"), "0.5.0"));
        assert!(hindsight_version_meets_minimum(Some("v0.5.6"), "0.5.0"));
        assert!(hindsight_version_meets_minimum(
            Some("0.6.0+local"),
            "0.5.0"
        ));
        assert!(!hindsight_version_meets_minimum(Some("0.4.99"), "0.5.0"));
        assert!(!hindsight_version_meets_minimum(
            Some("not-a-version"),
            "0.5.0"
        ));
        assert!(!hindsight_version_meets_minimum(None, "0.5.0"));
    }

    #[test]
    fn test_sync_turn_buffers_until_retain_threshold_then_drains() {
        let plugin = HindsightPlugin::new();
        *plugin.config.lock().unwrap() = Some(HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: "hybrid".into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 3,
            retain_context: "conversation".into(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        });

        plugin.sync_turn("u1", "a1", "session-1");
        assert_eq!(plugin.session_turns.lock().unwrap().len(), 1);
        plugin.sync_turn("u2", "a2", "session-1");
        assert_eq!(plugin.session_turns.lock().unwrap().len(), 2);
        plugin.sync_turn("u3", "a3", "session-1");
        assert!(plugin.session_turns.lock().unwrap().is_empty());
    }

    #[test]
    fn test_session_switch_flushes_pending_turns_and_clears_prefetch() {
        let plugin = HindsightPlugin::new();
        *plugin.config.lock().unwrap() = Some(HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: "hybrid".into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 10,
            retain_context: "conversation".into(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        });
        *plugin.session_id.lock().unwrap() = "old-session".into();
        *plugin.document_id.lock().unwrap() = "old-doc".into();
        *plugin.prefetch_result.lock().unwrap() = "stale context".into();
        plugin
            .session_turns
            .lock()
            .unwrap()
            .push(hindsight_turn_payload("u", "a", "2026-06-08T00:00:00Z"));

        plugin.on_session_switch("new-session", "old-session", false);

        assert!(plugin.session_turns.lock().unwrap().is_empty());
        assert_eq!(*plugin.session_id.lock().unwrap(), "new-session");
        assert!(plugin
            .document_id
            .lock()
            .unwrap()
            .starts_with("new-session-"));
        assert!(plugin.prefetch_result.lock().unwrap().is_empty());
        assert_eq!(*plugin.turn_counter.lock().unwrap(), 0);
    }

    #[test]
    fn test_initialize_scopes_document_id_per_lifecycle() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");
        let _timeout = EnvGuard::remove("HINDSIGHT_TIMEOUT");

        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(tmp.path(), &json!({"mode": "cloud", "api_key": "test"}));

        let plugin = HindsightPlugin::new();
        plugin.initialize("session-1", tmp.path().to_str().expect("tmp path"));
        let first = plugin.document_id.lock().unwrap().clone();
        plugin.initialize("session-1", tmp.path().to_str().expect("tmp path"));
        let second = plugin.document_id.lock().unwrap().clone();

        assert!(first.starts_with("session-1-"));
        assert!(second.starts_with("session-1-"));
        assert_ne!(first, second);
    }
}
