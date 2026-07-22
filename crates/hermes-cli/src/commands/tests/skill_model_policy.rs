use super::*;
use hermes_config::LlmProviderConfig;
use bytes::Bytes;
use std::path::Path;
use base64::Engine as _;
use sha2::{Digest as _, Sha256};

    #[test]
    fn read_skill_taps_accepts_upstream_object_shape() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("skill_taps.json");
        std::fs::write(
            &path,
            r#"{
  "taps": [
    { "repo": "MiniMax-AI/cli", "path": "skill/" },
    { "repo": "openai/skills", "path": "skills/" },
    { "repo": "anthropics/skills" },
    { "url": "https://github.com/garrytan/gstack::" }
  ]
}"#,
        )
        .expect("write");

        let taps = read_skill_taps(&path);
        assert!(taps.contains(&"https://github.com/MiniMax-AI/cli::skill".to_string()));
        assert!(taps.contains(&"https://github.com/openai/skills::skills".to_string()));
        assert!(taps.contains(&"https://github.com/anthropics/skills::skills".to_string()));
        assert!(taps.contains(&"https://github.com/garrytan/gstack::".to_string()));
    }

    #[test]
    fn write_skill_taps_writes_canonical_object_shape() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("skill_taps.json");
        let taps = vec![
            "https://github.com/MiniMax-AI/cli::skill".to_string(),
            "https://github.com/github/awesome-copilot::skills".to_string(),
            "https://github.com/garrytan/gstack::".to_string(),
        ];
        write_skill_taps(&path, &taps).expect("write taps");

        let raw = std::fs::read_to_string(&path).expect("read");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let arr = value
            .get("taps")
            .and_then(|v| v.as_array())
            .expect("taps array");
        assert_eq!(arr.len(), 3);

        let first = arr[0].as_object().expect("first object");
        assert_eq!(
            first.get("repo").and_then(|v| v.as_str()),
            Some("MiniMax-AI/cli")
        );
        assert_eq!(first.get("path").and_then(|v| v.as_str()), Some("skill/"));
    }

    #[test]
    fn read_skill_subscriptions_accepts_array_and_object_shapes() {
        let tmp = tempdir().expect("tempdir");
        let array_path = tmp.path().join("subscriptions-array.json");
        std::fs::write(
            &array_path,
            r#"[
  { "source": "https://github.com/example/skills::skills", "added_at": "now" },
  { "url": "https://github.com/example/more-skills::skills" },
  "https://github.com/example/string-entry::skills"
]"#,
        )
        .expect("write array shape");
        let arr = read_skill_subscriptions(&array_path);
        assert!(arr.contains(&"https://github.com/example/skills::skills".to_string()));
        assert!(arr.contains(&"https://github.com/example/more-skills::skills".to_string()));
        assert!(arr.contains(&"https://github.com/example/string-entry::skills".to_string()));

        let object_path = tmp.path().join("subscriptions-object.json");
        std::fs::write(
            &object_path,
            r#"{
  "subscriptions": [
    { "tap": "https://github.com/example/object-shape::skills" }
  ]
}"#,
        )
        .expect("write object shape");
        let obj = read_skill_subscriptions(&object_path);
        assert_eq!(
            obj,
            vec!["https://github.com/example/object-shape::skills".to_string()]
        );
    }

    #[test]
    fn effective_skill_taps_merges_defaults_custom_and_subscriptions() {
        let tmp = tempdir().expect("tempdir");
        let taps_file = tmp.path().join("skill_taps.json");
        let subscriptions_file = tmp.path().join("subscriptions.json");

        write_skill_taps(
            &taps_file,
            &["https://github.com/example/custom-skills::skills".to_string()],
        )
        .expect("write taps");
        std::fs::write(
            &subscriptions_file,
            r#"[
  { "source": "https://github.com/example/subscribed-skills::skills" },
  { "source": "not-a-tap-registry://ignored" }
]"#,
        )
        .expect("write subscriptions");

        let merged = effective_skill_taps(&taps_file, &subscriptions_file);
        assert!(merged.contains(&"https://github.com/openai/skills::skills".to_string()));
        assert!(merged.contains(&"https://github.com/example/custom-skills::skills".to_string()));
        assert!(
            merged.contains(&"https://github.com/example/subscribed-skills::skills".to_string())
        );
        assert!(!merged.contains(&"not-a-tap-registry://ignored".to_string()));
    }

    #[test]
    fn subscription_source_to_tap_filters_registry_prefixes_and_non_github_schemes() {
        assert_eq!(
            subscription_source_to_tap("https://github.com/example/skills::skills"),
            Some("https://github.com/example/skills::skills".to_string())
        );
        assert_eq!(subscription_source_to_tap("official/coder"), None);
        assert_eq!(subscription_source_to_tap("skills.sh/foo/bar"), None);
        assert_eq!(
            subscription_source_to_tap("not-a-tap-registry://ignored"),
            None
        );
    }

    #[test]
    fn sort_registry_skill_records_uses_router_priority_tie_break() {
        let mut records = vec![
            RegistrySkillRecord {
                identifier: "lobehub/a".to_string(),
                description: "".to_string(),
                source: "lobehub".to_string(),
                score: 700,
                install_source: RegistryInstallSource::LobeRegistry {
                    slug: "a".to_string(),
                },
            },
            RegistrySkillRecord {
                identifier: "skills.sh/b".to_string(),
                description: "".to_string(),
                source: "skills.sh".to_string(),
                score: 700,
                install_source: RegistryInstallSource::GitRepo {
                    repo: "openai/skills".to_string(),
                    requested_ref: None,
                    skill_dir: "skills/b".to_string(),
                },
            },
            RegistrySkillRecord {
                identifier: "github/c".to_string(),
                description: "".to_string(),
                source: "github".to_string(),
                score: 700,
                install_source: RegistryInstallSource::GitRepo {
                    repo: "openai/skills".to_string(),
                    requested_ref: None,
                    skill_dir: "skills/c".to_string(),
                },
            },
        ];

        sort_registry_skill_records(&mut records);
        let ordered_sources: Vec<String> = records.into_iter().map(|r| r.source).collect();
        assert_eq!(
            ordered_sources,
            vec![
                "skills.sh".to_string(),
                "github".to_string(),
                "lobehub".to_string()
            ]
        );
    }

    #[test]
    fn parse_explicit_github_skill_owner_repo_path() {
        let parsed = parse_explicit_github_skill("openai/skills/skills/.system/skill-creator")
            .expect("explicit parse");
        assert_eq!(parsed.0, "openai/skills");
        assert_eq!(parsed.1, None);
        assert_eq!(parsed.2, "skills/.system/skill-creator");
    }

    #[test]
    fn registry_prefixed_install_identifiers_override_github_slug_parse() {
        let registry_prefixed = parse_registry_prefixed_skill("official/creative/comfyui");
        assert_eq!(
            registry_prefixed,
            Some(("official".to_string(), "creative/comfyui".to_string()))
        );
        let explicit = if registry_prefixed.is_some() {
            None
        } else {
            parse_explicit_github_skill("official/creative/comfyui")
        };
        assert!(explicit.is_none());
    }

    #[test]
    fn registry_prefixed_install_identifiers_override_github_slug_parse_pretext() {
        let registry_prefixed = parse_registry_prefixed_skill("official/creative/pretext");
        assert_eq!(
            registry_prefixed,
            Some(("official".to_string(), "creative/pretext".to_string()))
        );
        assert!(parse_explicit_github_skill("official/creative/pretext").is_none());
    }

    #[test]
    fn parse_skill_name_and_version_handles_repo_plus_skill() {
        let (name, suffix) = parse_skill_name_and_version("openai/skills@skill-creator");
        assert_eq!(name, "openai/skills");
        assert_eq!(suffix.as_deref(), Some("skill-creator"));
        assert!(looks_like_github_repo_slug(&name));
    }

    #[test]
    fn sanitize_skill_install_name_normalizes_path_tail() {
        assert_eq!(
            sanitize_skill_install_name("skills/.system/skill-creator"),
            "skill-creator"
        );
        assert_eq!(sanitize_skill_install_name("bad$name"), "bad_name");
    }

    #[test]
    fn ensure_safe_relative_path_rejects_traversal() {
        assert!(ensure_safe_relative_path("SKILL.md").is_ok());
        assert!(ensure_safe_relative_path("../SKILL.md").is_err());
        assert!(ensure_safe_relative_path("nested/../../bad").is_err());
    }


    #[test]
    fn parse_model_switch_request_picks_provider_when_empty() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&[], &providers);
        assert_eq!(req, ModelSwitchRequest::PickProviderThenModel);
    }

    #[test]
    fn tail_text_lines_returns_last_n_lines() {
        let body = "a\nb\nc\nd\ne\n";
        assert_eq!(tail_text_lines(body, 2), "d\ne");
        assert_eq!(tail_text_lines(body, 10), "a\nb\nc\nd\ne");
    }

    #[test]
    fn backend_profile_lookup_resolves_aliases() {
        let row = backend_profile_lookup("llvm", Some("throughput")).expect("profile");
        assert_eq!(row.provider, "vllm");
        assert_eq!(row.profile, "throughput");

        let row = backend_profile_lookup("lm-studio", None).expect("lmstudio profile");
        assert_eq!(row.provider, "lmstudio");
        let row = backend_profile_lookup("oobabooga", None).expect("textgen profile");
        assert_eq!(row.provider, "text-generation-webui");
        let row = backend_profile_lookup("exllamav2", None).expect("tabby profile");
        assert_eq!(row.provider, "tabbyapi");
        let row = backend_profile_lookup("vmlx", None).expect("mlx profile");
        assert_eq!(row.provider, "mlx");
    }

    #[test]
    fn extract_embedding_diag_line_supports_nested_payload() {
        let payload = serde_json::json!({
            "retrieval": {
                "embedding_backend": "qdrant",
                "embedding_model": "text-embedding-3-large",
                "embedding_dimension": 3072
            }
        });
        let line = extract_embedding_diag_line(&payload);
        assert!(line.contains("backend=qdrant"));
        assert!(line.contains("model=text-embedding-3-large"));
        assert!(line.contains("dimension=3072"));
    }

    #[tokio::test]
    async fn model_refresh_command_clears_cache_and_lists_configured_provider() {
        let _lock = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let mut cfg = (*app.config).clone();
        cfg.llm_providers.insert(
            "wide-provider".to_string(),
            LlmProviderConfig {
                models: vec!["model-a".to_string(), "model-b".to_string()],
                discover_models: false,
                ..LlmProviderConfig::default()
            },
        );
        app.config = Arc::new(cfg);

        handle_model_command(&mut app, &["refresh", "wide-provider"])
            .await
            .expect("refresh model catalog");

        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Model catalog refreshed"));
        assert!(out.contains("provider: wide-provider"));
        assert!(out.contains("known_provider: yes"));
        assert!(out.contains("cache_cleared: no"));
        assert!(out.contains("catalog_total: 2"));
        assert!(out.contains("models_sample: model-a, model-b"));
    }

    #[tokio::test]
    async fn model_persistence_preserves_nested_model_siblings() {
        let _lock = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let config_path = app.state_root.join("config.yaml");
        std::fs::write(
            &config_path,
            r#"
model:
  default: old-model
  provider: old-provider
  base_url: https://old.example.com/v1
  model_slots:
    primary: keep-me
llm_providers:
  anthropic:
    base_url: https://api.anthropic.com
"#,
        )
        .expect("write nested model config");

        let mut cfg = (*app.config).clone();
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                base_url: Some("https://api.anthropic.com".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        app.config = Arc::new(cfg);
        app.current_model = "anthropic:claude-sonnet-4-6".to_string();

        let persisted_path =
            persist_current_model_selection(&app).expect("persist model selection");
        assert_eq!(persisted_path, config_path);

        let raw: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let model = raw
            .get("model")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("model block preserved");
        assert_eq!(
            model
                .get(serde_yaml::Value::String("default".to_string()))
                .and_then(serde_yaml::Value::as_str),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            model
                .get(serde_yaml::Value::String("provider".to_string()))
                .and_then(serde_yaml::Value::as_str),
            Some("anthropic")
        );
        assert_eq!(
            model
                .get(serde_yaml::Value::String("base_url".to_string()))
                .and_then(serde_yaml::Value::as_str),
            Some("https://api.anthropic.com")
        );
        assert_eq!(
            model
                .get(serde_yaml::Value::String("model_slots".to_string()))
                .and_then(serde_yaml::Value::as_mapping)
                .and_then(|slots| {
                    slots
                        .get(serde_yaml::Value::String("primary".to_string()))
                        .and_then(serde_yaml::Value::as_str)
                }),
            Some("keep-me")
        );

        let loaded = hermes_config::load_user_config_file(&config_path).expect("load config");
        assert_eq!(loaded.model.as_deref(), Some("anthropic:claude-sonnet-4-6"));
    }

    #[tokio::test]
    async fn model_switch_command_emits_preflight_warning_for_large_transcript() {
        let _lock = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let mut cfg = (*app.config).clone();
        cfg.model = Some("anthropic:claude-sonnet-4-6".to_string());
        cfg.llm_providers.insert(
            "compact-provider".to_string(),
            LlmProviderConfig {
                models: vec!["deepseek-chat".to_string()],
                discover_models: false,
                ..LlmProviderConfig::default()
            },
        );
        app.config = Arc::new(cfg);
        app.current_model = "anthropic:claude-sonnet-4-6".to_string();
        app.messages = vec![hermes_core::Message::user("abcd".repeat(90_000))];

        handle_model_command(
            &mut app,
            &["deepseek-chat", "--provider", "compact-provider"],
        )
        .await
        .expect("model switch");

        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Model switched to: compact-provider:deepseek-chat"));
        assert!(out.contains("Context warning"));
        assert!(out.contains("preflight compression"));
        assert_eq!(
            app.messages.len(),
            1,
            "warning must not append model context"
        );
    }

    #[tokio::test]
    async fn model_switch_command_failure_does_not_commit_or_claim_success() {
        let _lock = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let mut cfg = (*app.config).clone();
        cfg.model = Some("anthropic:claude-sonnet-4-6".to_string());
        cfg.llm_providers.insert(
            "compact-provider".to_string(),
            LlmProviderConfig {
                models: vec!["deepseek-chat".to_string()],
                discover_models: false,
                ..LlmProviderConfig::default()
            },
        );
        app.config = Arc::new(cfg);
        app.try_switch_model("anthropic:claude-sonnet-4-6")
            .expect("baseline switch");
        app.force_model_rebuild_failure_for_test("compact-provider:deepseek-chat");

        handle_model_command(
            &mut app,
            &["deepseek-chat", "--provider", "compact-provider"],
        )
        .await
        .expect("model switch failure should be reported, not thrown");

        let out = latest_ui_assistant_text(&app);
        assert!(out.contains("Model switch to compact-provider:deepseek-chat failed"));
        assert!(out.contains("staying on anthropic:claude-sonnet-4-6"));
        assert!(!out.contains("Model switched to: compact-provider:deepseek-chat"));
        assert_eq!(app.current_model, "anthropic:claude-sonnet-4-6");
    }

    #[test]
    fn parse_model_command_args_extracts_capability_flags() {
        let (positional, requirements, provider_override) = parse_model_command_args(&[
            "nous",
            "--cap",
            "vision,reasoning",
            "--min-context",
            "200000",
        ])
        .expect("parse");
        assert_eq!(positional, vec!["nous".to_string()]);
        assert!(requirements.require_vision);
        assert!(requirements.require_reasoning);
        assert!(!requirements.require_tools);
        assert_eq!(requirements.min_context_window, Some(200_000));
        assert!(provider_override.is_none());
    }

    #[test]
    fn parse_model_command_args_supports_boolean_capability_switches() {
        let (positional, requirements, provider_override) =
            parse_model_command_args(&["nous:openai/gpt-5.5-pro", "--tools", "--long-context"])
                .expect("parse");
        assert_eq!(positional, vec!["nous:openai/gpt-5.5-pro".to_string()]);
        assert!(requirements.require_tools);
        assert!(requirements.require_long_context);
        assert_eq!(
            requirements.effective_min_context(),
            Some(ModelCapabilityRequirements::LONG_CONTEXT_DEFAULT)
        );
        assert!(provider_override.is_none());
    }

    #[test]
    fn parse_model_command_args_extracts_provider_override() {
        let (positional, _requirements, provider_override) =
            parse_model_command_args(&["dynamic", "--provider", "nous"]).expect("parse");
        assert_eq!(positional, vec!["dynamic".to_string()]);
        assert_eq!(provider_override.as_deref(), Some("nous"));
    }

    #[test]
    fn model_meets_requirements_checks_tools_vision_reasoning_and_context() {
        let requirements = ModelCapabilityRequirements {
            require_tools: true,
            require_vision: true,
            require_reasoning: true,
            require_long_context: false,
            min_context_window: Some(128_000),
        };
        let caps = ResolvedModelCapabilities {
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
            context_window: 200_000,
        };
        assert!(model_meets_requirements(caps, requirements));
        let weak_caps = ResolvedModelCapabilities {
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: true,
            context_window: 200_000,
        };
        assert!(!model_meets_requirements(weak_caps, requirements));
    }

    #[test]
    fn unmet_model_requirements_lists_missing_constraints() {
        let requirements = ModelCapabilityRequirements {
            require_tools: true,
            require_vision: true,
            require_reasoning: true,
            require_long_context: false,
            min_context_window: Some(256_000),
        };
        let caps = ResolvedModelCapabilities {
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: false,
            context_window: 128_000,
        };
        let missing = unmet_model_requirements(caps, requirements);
        assert!(missing.iter().any(|m| m == "vision"));
        assert!(missing.iter().any(|m| m == "reasoning"));
        assert!(missing
            .iter()
            .any(|m| m.contains("context>=256000 (actual=128000)")));
    }

    #[test]
    fn resolve_model_capabilities_uses_codex_oauth_display_context_cap() {
        let client = hermes_intelligence::models_dev::ModelsDevClient::new(
            "http://127.0.0.1:9",
            std::env::temp_dir().join("hermes-models-dev-codex-cap.json"),
        );
        let caps = resolve_model_capabilities("openai-codex", "gpt-5.5", &client, None);

        assert_eq!(caps.context_window, OPENAI_CODEX_OAUTH_CONTEXT_WINDOW);
    }

    #[test]
    fn resolve_model_capabilities_uses_custom_provider_context_override() {
        let client = hermes_intelligence::models_dev::ModelsDevClient::new(
            "http://127.0.0.1:9",
            std::env::temp_dir().join("hermes-models-dev-custom-context.json"),
        );
        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "my-custom-endpoint".to_string(),
            hermes_config::LlmProviderConfig {
                base_url: Some("https://example.invalid/v1".to_string()),
                models: vec!["gpt-5.5".to_string()],
                model_context_windows: std::collections::HashMap::from([(
                    "gpt-5.5".to_string(),
                    1_050_000,
                )]),
                ..hermes_config::LlmProviderConfig::default()
            },
        );

        let caps = resolve_model_capabilities(
            "my-custom-endpoint",
            "gpt-5.5",
            &client,
            Some(&config),
        );

        assert_eq!(caps.context_window, 1_050_000);
    }

    #[test]
    fn parse_model_command_args_rejects_unknown_capability() {
        let err = parse_model_command_args(&["--cap", "telepathy"]).expect_err("expected error");
        let message = err.to_string().to_ascii_lowercase();
        assert!(message.contains("unknown model capability"));
    }

    #[test]
    fn policy_profile_resolution_accepts_primary_aliases() {
        assert_eq!(
            resolve_policy_profile("strict").map(|p| p.name),
            Some("strict")
        );
        assert_eq!(
            resolve_policy_profile("standard").map(|p| p.name),
            Some("standard")
        );
        assert_eq!(
            resolve_policy_profile("balanced").map(|p| p.name),
            Some("standard")
        );
        assert_eq!(resolve_policy_profile("dev").map(|p| p.name), Some("dev"));
        assert!(resolve_policy_profile("unknown").is_none());
    }

    #[test]
    fn legacy_replay_trace_diagnostic_detects_hash_break() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("session.jsonl");
        std::fs::write(
            &path,
            r#"{"seq":1,"event":"user","prev_hash":"seed","event_hash":"h1","payload":{"turn":1}}
{"seq":2,"event":"assistant","prev_hash":"BROKEN","event_hash":"h2","payload":{"turn":1}}
"#,
        )
        .expect("write replay");
        let (entries, parse_errors, chain_breaks) =
            legacy_replay_trace_diagnostic(&path).expect("diagnostic");
        assert_eq!(entries, 2);
        assert_eq!(parse_errors, 0);
        assert_eq!(chain_breaks, 1);
    }

    #[test]
    fn parse_model_switch_request_uses_provider_picker_for_provider_arg() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["NOUS"], &providers);
        assert_eq!(
            req,
            ModelSwitchRequest::PickModelFromProvider("nous".to_string())
        );
    }

    #[test]
    fn parse_model_switch_request_accepts_configured_provider_strings() {
        let providers = vec![
            "openai".to_string(),
            "qianfan-coding".to_string(),
            "my-gateway".to_string(),
        ];
        let req = parse_model_switch_request(&["QIANFAN-CODING"], &providers);
        assert_eq!(
            req,
            ModelSwitchRequest::PickModelFromProvider("qianfan-coding".to_string())
        );
    }

    #[tokio::test]
    async fn guard_provider_model_selection_uses_configured_provider_models() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "qianfan-coding".to_string(),
            LlmProviderConfig {
                models: vec!["kimi-k2.5".to_string(), "glm-5".to_string()],
                discover_models: false,
                ..LlmProviderConfig::default()
            },
        );

        let (guarded, note) =
            guard_provider_model_selection_for_config("qianfan-coding:kimi", &cfg)
                .await
                .expect("guard");

        assert_eq!(guarded, "qianfan-coding:kimi-k2.5");
        assert!(note
            .as_deref()
            .is_some_and(|text| text.contains("Model catalog guard remapped")));
    }

    #[test]
    fn parse_model_switch_request_accepts_direct_provider_model() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["nous:openai/gpt-5.5-pro"], &providers);
        assert_eq!(
            req,
            ModelSwitchRequest::SetDirect("nous:openai/gpt-5.5-pro".to_string())
        );
    }

    #[test]
    fn parse_model_switch_request_keeps_bare_model_as_direct() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["dynamic"], &providers);
        assert_eq!(req, ModelSwitchRequest::SetDirect("dynamic".to_string()));
    }

    #[test]
    fn normalize_model_target_uses_current_provider_for_bare_model() {
        let normalized = normalize_model_target("nous:moonshotai/kimi-k2.6", "openai/gpt-5.5")
            .expect("normalize");
        assert_eq!(normalized, "nous:openai/gpt-5.5");
    }

    #[test]
    fn normalize_model_target_keeps_explicit_provider_model() {
        let normalized = normalize_model_target("nous:moonshotai/kimi-k2.6", "openai:gpt-5.4")
            .expect("normalize");
        assert_eq!(normalized, "openai:gpt-5.4");
    }

    #[test]
    fn parse_toggle_arg_supports_status_and_explicit_values() {
        assert!(!parse_toggle_arg(None, true).expect("toggle"));
        assert!(parse_toggle_arg(Some("toggle"), false).expect("toggle"));
        assert!(parse_toggle_arg(Some("on"), false).expect("on"));
        assert!(!parse_toggle_arg(Some("off"), true).expect("off"));
        assert!(parse_toggle_arg(Some("bad-value"), true).is_err());
    }

    #[test]
    fn parse_reasoning_effort_accepts_levels_and_auto_clear() {
        assert_eq!(
            parse_reasoning_effort("minimal").expect("minimal"),
            Some("minimal")
        );
        assert_eq!(parse_reasoning_effort("low").expect("low"), Some("low"));
        assert_eq!(
            parse_reasoning_effort("medium").expect("medium"),
            Some("medium")
        );
        assert_eq!(parse_reasoning_effort("high").expect("high"), Some("high"));
        assert_eq!(
            parse_reasoning_effort("xhigh").expect("xhigh"),
            Some("xhigh")
        );
        assert_eq!(parse_reasoning_effort("auto").expect("auto"), None);
        assert!(parse_reasoning_effort("turbo").is_err());
    }

    #[test]
    fn set_provider_reasoning_effort_updates_and_clears_extra_body() {
        let mut cfg = GatewayConfig::default();
        set_provider_reasoning_effort(&mut cfg, "nous", Some("high"));
        let extra = cfg
            .llm_providers
            .get("nous")
            .and_then(|entry| entry.extra_body.as_ref())
            .expect("extra body");
        assert!(extra.get("reasoning_effort").is_none());
        assert_eq!(
            extra
                .get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(|value| value.as_str())
                .expect("reasoning.effort"),
            "high"
        );

        set_provider_reasoning_effort(&mut cfg, "nous", None);
        let extra_after_clear = cfg
            .llm_providers
            .get("nous")
            .and_then(|entry| entry.extra_body.as_ref());
        assert!(extra_after_clear.is_none());
    }

    #[test]
    fn set_provider_reasoning_effort_normalizes_openai_effort_levels() {
        let mut cfg = GatewayConfig::default();
        set_provider_reasoning_effort(&mut cfg, "nous", Some("xhigh"));
        let extra = cfg
            .llm_providers
            .get("nous")
            .and_then(|entry| entry.extra_body.as_ref())
            .expect("extra body");
        assert_eq!(
            extra
                .get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(|value| value.as_str()),
            Some("high")
        );
        set_provider_reasoning_effort(&mut cfg, "nous", Some("minimal"));
        let extra = cfg
            .llm_providers
            .get("nous")
            .and_then(|entry| entry.extra_body.as_ref())
            .expect("extra body");
        assert_eq!(
            extra
                .get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(|value| value.as_str()),
            Some("low")
        );
    }

    #[test]
    fn set_provider_reasoning_effort_preserves_opencode_go_levels_for_runtime_mapping() {
        let mut cfg = GatewayConfig::default();
        set_provider_reasoning_effort(&mut cfg, "opencode-go", Some("xhigh"));
        let extra = cfg
            .llm_providers
            .get("opencode-go")
            .and_then(|entry| entry.extra_body.as_ref())
            .expect("extra body");
        assert_eq!(
            extra
                .get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(|value| value.as_str()),
            Some("xhigh")
        );

        set_provider_reasoning_effort(&mut cfg, "opencode-go", Some("minimal"));
        let extra = cfg
            .llm_providers
            .get("opencode-go")
            .and_then(|entry| entry.extra_body.as_ref())
            .expect("extra body");
        assert_eq!(
            extra
                .get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(|value| value.as_str()),
            Some("minimal")
        );
    }

    #[test]
    fn set_provider_reasoning_effort_sets_gemini_thinking_level() {
        let mut cfg = GatewayConfig::default();
        set_provider_reasoning_effort(&mut cfg, "gemini", Some("xhigh"));
        let extra = cfg
            .llm_providers
            .get("gemini")
            .and_then(|entry| entry.extra_body.as_ref())
            .expect("extra body");
        assert_eq!(
            extra
                .get("google")
                .and_then(|value| value.get("thinking_config"))
                .and_then(|value| value.get("thinking_level"))
                .and_then(|value| value.as_str()),
            Some("high")
        );
        assert_eq!(
            extra
                .get("thinking_config")
                .and_then(|value| value.get("thinking_level"))
                .and_then(|value| value.as_str()),
            Some("high")
        );
    }

    #[test]
    fn parse_pet_species_and_mood_validate_catalog_entries() {
        assert_eq!(parse_pet_species("fox").as_deref(), Some("fox"));
        assert!(parse_pet_species("dragon").is_none());
        assert_eq!(parse_pet_mood("ready").as_deref(), Some("ready"));
        assert!(parse_pet_mood("sleeping-beauty").is_none());
    }

    #[test]
    fn parse_pet_dock_accepts_left_or_right() {
        assert_eq!(parse_pet_dock("left"), Some(PetDock::Left));
        assert_eq!(parse_pet_dock("right"), Some(PetDock::Right));
        assert_eq!(parse_pet_dock("center"), None);
    }

    #[test]
    fn format_personality_catalog_includes_current_and_usage_hint() {
        let catalog = format_personality_catalog(
            Some("technical"),
            &[("coder", "Use when building or debugging code.")],
        );
        assert!(catalog.contains("## Built-in personalities"));
        assert!(catalog.contains("Current: `technical`"));
        assert!(catalog.contains("Use `/personality <name>` to switch."));
    }

    #[test]
    fn format_personality_catalog_renders_multiline_entries() {
        let catalog = format_personality_catalog(
            None,
            &[
                ("coder", "Use when building or debugging code."),
                ("writer", "Use when drafting polished prose."),
            ],
        );
        assert!(catalog.contains("- `coder`\n  Use when building or debugging code."));
        assert!(catalog.contains("- `writer`\n  Use when drafting polished prose."));
    }

    #[test]
    fn secret_stdout_gate_defaults_false() {
        let _lock = env_test_lock();
        std::env::remove_var("HERMES_ALLOW_SECRET_STDOUT");
        assert!(!secret_stdout_allowed());
    }

    #[test]
    fn secret_stdout_gate_accepts_truthy_values() {
        let _lock = env_test_lock();
        std::env::set_var("HERMES_ALLOW_SECRET_STDOUT", "yes");
        assert!(secret_stdout_allowed());
        std::env::remove_var("HERMES_ALLOW_SECRET_STDOUT");
    }

    #[test]
    fn mask_secret_value_hides_payload() {
        let raw = "very-secret-value";
        let masked = mask_secret_value(raw);
        assert!(!masked.contains(raw));
        assert!(masked.contains("***"));
    }

    #[test]
    fn mask_secret_value_handles_multibyte_secrets() {
        let raw = "aééééééééz";
        let masked = mask_secret_value(raw);
        assert!(!masked.contains(raw));
        assert!(masked.contains("***"));
    }

    #[test]
    fn redacted_config_json_masks_typed_and_nested_secrets() {
        let mut config = GatewayConfig::default();
        config.terminal.docker_env = Some("OPENAI_API_KEY=sk-live-secret".to_string());
        config.proxy = Some(hermes_config::ProxyConfig {
            http_proxy: Some(
                "http://alice:proxy-secret@proxy.example.test:8080?token=query-secret".to_string(),
            ),
            ..Default::default()
        });
        let mut platform = hermes_config::PlatformConfig {
            enabled: true,
            token: Some("typed-platform-token".to_string()),
            webhook_url: Some(
                "https://hooks.example.test/services/credential-value".to_string(),
            ),
            ..Default::default()
        };
        platform.extra.insert(
            "provider_options".to_string(),
            serde_json::json!({
                "nested": {
                    "customApiKey": "nested-provider-secret",
                    "api-key": "hyphenated-provider-secret",
                    "encrypt_key": "feishu-encryption-key",
                    "encoding_aes_key": "wecom-callback-key",
                    "numeric_token": 424242,
                    "secret_enabled": true,
                    "max_tokens": 4096,
                    "oauth_token_url": "https://auth.example.test/token",
                    "api_key_env": "EXAMPLE_API_KEY"
                }
            }),
        );
        config.platforms.insert("example".to_string(), platform);

        let rendered = redacted_config_json(&config).expect("render redacted config");
        let value: serde_json::Value =
            serde_json::from_str(&rendered).expect("parse redacted config");
        let platform = &value["platforms"]["example"];
        let nested = &platform["provider_options"]["nested"];
        assert_eq!(platform["token"], REDACTED_CONFIG_SECRET);
        assert_eq!(platform["webhook_url"], REDACTED_CONFIG_SECRET);
        assert_eq!(nested["customApiKey"], REDACTED_CONFIG_SECRET);
        assert_eq!(nested["api-key"], REDACTED_CONFIG_SECRET);
        assert_eq!(nested["encrypt_key"], REDACTED_CONFIG_SECRET);
        assert_eq!(nested["encoding_aes_key"], REDACTED_CONFIG_SECRET);
        assert_eq!(nested["numeric_token"], REDACTED_CONFIG_SECRET);
        assert_eq!(nested["secret_enabled"], REDACTED_CONFIG_SECRET);
        assert_eq!(nested["max_tokens"], 4096);
        assert_eq!(
            nested["oauth_token_url"],
            "https://auth.example.test/token"
        );
        assert_eq!(nested["api_key_env"], "EXAMPLE_API_KEY");
        assert_eq!(value["terminal"]["docker_env"], REDACTED_CONFIG_SECRET);
        let proxy = value["proxy"]["http_proxy"]
            .as_str()
            .expect("redacted proxy URL");
        assert!(proxy.contains("proxy.example.test"));
        assert!(!proxy.contains("alice"));
        assert!(!proxy.contains("proxy-secret"));
        assert!(!proxy.contains("query-secret"));
    }

    #[test]
    fn resolve_catalog_model_candidate_prefers_suffix_match() {
        let catalog = vec![
            "nousresearch/hermes-4-405b".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let chosen = resolve_catalog_model_candidate("kimi-k2.6", &catalog).expect("candidate");
        assert_eq!(chosen, "moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_catalog_model_candidate_uses_relative_match_for_near_miss() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let chosen = resolve_catalog_model_candidate("qwen3.6-max", &catalog).expect("candidate");
        assert_eq!(chosen, "qwen/qwen3.6-max-preview");
    }

    #[test]
    fn rank_catalog_model_candidates_returns_best_first() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let ranked = rank_catalog_model_candidates("qwen3.6-max", &catalog, 2);
        assert_eq!(
            ranked,
            vec![
                "qwen/qwen3.6-max-preview".to_string(),
                "qwen/qwen3.6-plus".to_string()
            ]
        );
    }

    #[test]
    fn skills_action_blocked_by_tier_enforces_expected_matrix() {
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "install",
            None
        ));
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "tap",
            Some("add")
        ));
        assert!(!skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "list",
            None
        ));
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Balanced,
            "publish",
            None
        ));
        assert!(!skills_action_blocked_by_tier(
            SkillsExecutionTier::Balanced,
            "install",
            None
        ));
        assert!(!skills_action_blocked_by_tier(
            SkillsExecutionTier::Open,
            "publish",
            None
        ));
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "sync",
            None
        ));
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "opt-in",
            Some("--sync")
        ));
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Balanced,
            "opt-out",
            Some("--remove")
        ));
        assert!(!skills_action_blocked_by_tier(
            SkillsExecutionTier::Balanced,
            "opt-out",
            None
        ));
    }

    #[test]
    fn parse_skills_slash_invocation_supports_blank_slate_commands() {
        let sync = parse_skills_slash_invocation(&["sync"]).expect("sync");
        assert_eq!(sync.action.as_deref(), Some("sync"));
        assert_eq!(sync.name, None);

        let opt_out =
            parse_skills_slash_invocation(&["opt-out", "--remove", "--yes"]).expect("opt-out");
        assert_eq!(opt_out.action.as_deref(), Some("opt-out"));
        assert_eq!(opt_out.name, None);
        assert_eq!(opt_out.extra.as_deref(), Some("--remove --yes"));

        let opt_in = parse_skills_slash_invocation(&["opt-in", "--sync"]).expect("opt-in");
        assert_eq!(opt_in.action.as_deref(), Some("opt-in"));
        assert_eq!(opt_in.extra.as_deref(), Some("--sync"));
    }

    #[test]
    fn specpatch_block_reason_flags_destructive_patterns() {
        assert!(specpatch_block_reason("echo safe").is_none());
        assert!(specpatch_block_reason("rm -rf /").is_some());
        assert!(specpatch_block_reason("rm -rf /tmp").is_some());
        assert!(specpatch_block_reason("git reset --hard HEAD").is_some());
    }

    #[test]
    fn extract_marker_paths_captures_path_and_file_tokens() {
        let text = "PATCH_VERIFIED: path=/tmp/a.rs file=src/main.rs cmd=rg -n foo";
        let paths = extract_marker_paths(text);
        assert!(paths.contains(&"/tmp/a.rs".to_string()));
        assert!(paths.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn normalize_repo_relative_path_handles_absolute_and_relative() {
        let root = PathBuf::from("/tmp/repo");
        let rel = normalize_repo_relative_path(&root, "src/main.rs").expect("relative");
        assert_eq!(rel, "src/main.rs");
        let abs = normalize_repo_relative_path(&root, "/tmp/repo/src/lib.rs").expect("abs");
        assert_eq!(abs, "src/lib.rs");
    }

    fn fixture_revision() -> &'static str {
        "0123456789abcdef0123456789abcdef01234567"
    }

    fn write_fixture_bundle(root: &Path, body: &str) -> Vec<(String, Bytes)> {
        std::fs::create_dir_all(root).expect("create fixture root");
        std::fs::write(root.join("SKILL.md"), body).expect("write fixture skill");
        vec![("SKILL.md".to_string(), Bytes::from(body.as_bytes().to_vec()))]
    }

    fn skill_document(name: &str, body: &str) -> String {
        format!(
            "---\nname: {name}\nversion: 1.0.0\ndescription: fixture\n---\n{body}"
        )
    }

    fn signed_remote_manifest(
        root: &Path,
        name: &str,
        origin: &str,
        revision: &str,
        signer: &hermes_skills::SkillSigningKey,
        permissions: hermes_skills::SkillPermissions,
    ) -> hermes_skills::SkillPackageManifestV2 {
        let identity = hermes_skills::SkillPackageIdentityV2::new(
            name,
            "1.0.0",
            origin,
            "fixture-publisher",
            revision,
            hermes_skills::canonical_bundle_digest(root).expect("fixture digest"),
            "2026-07-20T00:00:00Z",
        );
        let mut manifest = hermes_skills::SkillPackageManifestV2::new(
            hermes_skills::SkillPackageSource::Remote,
            identity,
            hermes_skills::SkillPackageProof::RemoteSignature(
                hermes_skills::SkillSignature::ed25519("fixture-key", ""),
            ),
            hermes_skills::full_skill_scan(root),
            hermes_skills::SkillCompatibility::default(),
            permissions,
            Vec::new(),
        );
        signer
            .sign_manifest(&mut manifest, "fixture-key")
            .expect("sign fixture manifest");
        manifest
    }


    fn fixture_authority(
        skills_root: &Path,
        authority_dir: &Path,
        signer: &hermes_skills::SkillSigningKey,
    ) -> hermes_skills::SkillTrustAuthorityPaths {
        std::fs::create_dir_all(skills_root).expect("create skills root");
        std::fs::create_dir_all(authority_dir).expect("create authority directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(skills_root, std::fs::Permissions::from_mode(0o700))
                .expect("restrict skills root");
            std::fs::set_permissions(authority_dir, std::fs::Permissions::from_mode(0o700))
                .expect("restrict authority directory");
        }
        let root_file = authority_dir.join("remote-trust-root.json");
        let audit_file = authority_dir.join("remote-keyring-audit.json");
        let owner_file = authority_dir.join("local-owner-keyring.json");
        hermes_skills::store_remote_trust_root_file(
            skills_root,
            &root_file,
            &signer.remote_trust_root("fixture-root", 1),
        )
        .expect("store fixture root");
        hermes_skills::store_local_owner_keyring_file(
            skills_root,
            &owner_file,
            &hermes_skills::LocalOwnerKeyring::new(Vec::new()).expect("empty owner keyring"),
        )
        .expect("store fixture owner keyring");
        let mut keyring = hermes_skills::SignedRemoteSkillKeyring::new(
            1,
            vec![signer.trusted_publisher_key(
                "fixture-publisher",
                "fixture-key",
                true,
            )],
        )
        .expect("fixture remote keyring");
        signer
            .sign_remote_keyring(&mut keyring, "fixture-root")
            .expect("sign fixture remote keyring");
        hermes_skills::store_signed_remote_keyring(skills_root, &keyring)
            .expect("store fixture remote keyring");
        hermes_skills::SkillTrustAuthorityPaths::new(
            skills_root.to_path_buf(),
            root_file,
            audit_file,
            owner_file,
        )
        .expect("fixture authority paths")
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn write_owner_only(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).expect("write fixture authority input");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("restrict fixture authority input");
        }
    }

    fn restrict_owner_only_directory(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("restrict fixture directory");
        }
    }

    #[test]
    fn signed_filesystem_package_stages_verifies_and_locks_v2() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("signed-source");
        let files = write_fixture_bundle(
            &source,
            "---\nname: signed\nversion: 1.0.0\ndescription: safe\n---\n# Signed skill\n",
        );
        let signer = hermes_skills::SkillSigningKey::from_seed([7; 32]);
        let manifest = signed_remote_manifest(
            &source,
            "signed",
            "github://fixture/signed",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let skills_root = tmp.path().join("skills");
        let authority = fixture_authority(&skills_root, &tmp.path().join("authority"), &signer);
        let admission = install_verified_skill_material(
            &skills_root,
            SkillInstallMaterial {
                install_name: "signed".to_string(),
                manifest,
                files,
            },
            &authority,
            false,
        )
        .expect("signed install");

        assert!(admission.is_allowed());
        let lock = hermes_skills::load_lock(&skills_root).expect("v2 lock");
        assert!(skills_root
            .join(&lock.packages[0].install_path)
            .join("SKILL.md")
            .is_file());
        assert_eq!(lock.schema_version, hermes_skills::SKILL_LOCK_SCHEMA_VERSION);
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.packages[0].manifest.identity.name, "signed");
        assert_eq!(
            skill_package_trust_label(&lock.packages[0].manifest),
            "remote-signed:key=fixture-key"
        );
        let entry = &lock.packages[0];
        assert_eq!(entry.manifest.identity.origin, "github://fixture/signed");
        assert_eq!(
            entry.manifest.identity.content_digest,
            hermes_skills::canonical_bundle_digest(&source).expect("canonical source digest")
        );
        assert!(!entry.manifest.identity.manifest_digest.is_empty());
        assert!(!entry.manifest.identity.installed_at.is_empty());
        assert_eq!(
            entry.manifest.scan.verdict,
            hermes_skills::SkillScanVerdictV2::Clean
        );
        assert_eq!(entry.manifest.scan.finding_count, 0);
    }

    #[test]
    fn unsigned_and_bad_signature_packages_leave_no_executable_tree() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source");
        let files = write_fixture_bundle(&source, &skill_document("rejected", "# Safe\n"));
        let signer = hermes_skills::SkillSigningKey::from_seed([8; 32]);
        let good = signed_remote_manifest(
            &source,
            "rejected",
            "github://fixture/rejected",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let mut unsigned = good.clone();
        unsigned.proof = hermes_skills::SkillPackageProof::LocalOwnerAttestation(
            hermes_skills::LocalOwnerAttestation::new(
                "owner",
                "manual",
                unsigned.identity.content_digest.clone(),
                "2026-07-20T00:00:00Z",
            ),
        );
        unsigned
            .refresh_manifest_digest()
            .expect("refresh unsigned manifest");
        let skills_root = tmp.path().join("skills");
        let authority = fixture_authority(&skills_root, &tmp.path().join("authority"), &signer);
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "rejected".to_string(),
                    manifest: unsigned,
                    files: files.clone(),
                },
                &authority,
                false,
            )
            .is_err()
        );
        assert!(hermes_skills::load_lock(&skills_root)
            .expect("read empty lock")
            .packages
            .is_empty());

        let mut bad_signature = good;
        if let hermes_skills::SkillPackageProof::RemoteSignature(signature) =
            &mut bad_signature.proof
        {
            signature.signature = "not-a-valid-ed25519-signature".to_string();
        }
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "rejected".to_string(),
                    manifest: bad_signature,
                    files,
                },
                &authority,
                false,
            )
            .is_err()
        );
        assert!(hermes_skills::load_lock(&skills_root)
            .expect("read empty lock")
            .packages
            .is_empty());
    }

    #[test]
    fn incompatible_and_overpermissioned_packages_are_denied_before_cutover() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("policy-source");
        let files = write_fixture_bundle(&source, &skill_document("policy", "# Safe\n"));
        let signer = hermes_skills::SkillSigningKey::from_seed([15; 32]);
        let skills_root = tmp.path().join("skills");
        let authority = fixture_authority(&skills_root, &tmp.path().join("authority"), &signer);

        let overpermissioned = signed_remote_manifest(
            &source,
            "policy",
            "github://fixture/policy",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions {
                requested: std::collections::BTreeSet::from([
                    hermes_skills::SkillPermission::Process,
                ]),
            },
        );
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "policy".to_string(),
                    manifest: overpermissioned,
                    files: files.clone(),
                },
                &authority,
                false,
            )
            .is_err()
        );

        let mut incompatible = signed_remote_manifest(
            &source,
            "policy",
            "github://fixture/policy",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        incompatible.compatibility.minimum_runtime_version = Some("999.0.0".to_string());
        signer
            .sign_manifest(&mut incompatible, "fixture-key")
            .expect("re-sign incompatible fixture");
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "policy".to_string(),
                    manifest: incompatible,
                    files,
                },
                &authority,
                false,
            )
            .is_err()
        );
        assert!(hermes_skills::load_lock(&skills_root)
            .expect("empty lock after policy denial")
            .packages
            .is_empty());
    }

    #[tokio::test]
    async fn mutable_github_ref_is_refused_by_local_http_fixture() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _guard = env_test_lock();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/fixture/demo/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sha": "main"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/fixture/demo/commits/{}",
                fixture_revision()
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sha": fixture_revision()
            })))
            .mount(&server)
            .await;
        let _api = EnvVarGuard::set("HERMES_SKILL_GITHUB_API_BASE", server.uri());
        let error = github_resolve_immutable_revision(
            &reqwest::Client::new(),
            "fixture/demo",
            Some("main"),
        )
        .await
        .expect_err("mutable ref must be rejected");
        assert!(error.to_string().contains("immutable"));
        assert_eq!(
            github_resolve_immutable_revision(
                &reqwest::Client::new(),
                "fixture/demo",
                Some(fixture_revision()),
            )
            .await
            .expect("full commit revision"),
            fixture_revision()
        );
    }

    #[test]
    fn local_fixture_source_requires_an_absolute_full_content_revision() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(parse_local_skill_source("file:///tmp/fixture-skill").is_err());
        assert!(
            parse_local_skill_source("file://relative/fixture?revision=sha256:abc").is_err()
        );
        let (path, revision) = parse_local_skill_source(&format!(
            "file:///tmp/fixture-skill?revision={digest}"
        ))
        .expect("fully pinned fixture source")
        .expect("local source");
        assert_eq!(path, PathBuf::from("/tmp/fixture-skill"));
        assert_eq!(revision, digest);
    }

    #[tokio::test]
    async fn cli_trust_provisions_pinned_root_and_accepts_old_root_authorized_rotation() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        restrict_owner_only_directory(tmp.path());
        let _home = TempHomeGuard::new(tmp.path());
        let skills_root = tmp.path().join("skills");
        let authority_dir = tmp.path().join("authority");

        handle_cli_skills_with_trust_args(
            Some("trust".to_string()),
            Some("init-local-owner-keyring".to_string()),
            None,
            Some(authority_dir.clone()),
            None,
            None,
            false,
            false,
            false,
        )
        .await
        .expect("initialize owner keyring");

        let old_root_signer = hermes_skills::SkillSigningKey::from_seed([31; 32]);
        let old_root = old_root_signer.remote_trust_root("old-root", 1);
        let root_source = tmp.path().join("old-root.json");
        let root_bytes = serde_json::to_vec(&old_root).expect("serialize old root");
        write_owner_only(&root_source, &root_bytes);
        handle_cli_skills_with_trust_args(
            Some("trust".to_string()),
            Some("provision-remote-root".to_string()),
            None,
            Some(authority_dir.clone()),
            Some(root_source.clone()),
            Some(sha256_hex(&root_bytes)),
            false,
            false,
            false,
        )
        .await
        .expect("provision pinned root");
        let persisted_root = std::fs::read(authority_dir.join("remote-trust-root.json"))
            .expect("read persisted root");
        assert_eq!(persisted_root, root_bytes);

        let replacement_source = tmp.path().join("replacement-root.json");
        let replacement_root =
            hermes_skills::SkillSigningKey::from_seed([32; 32]).remote_trust_root("other-root", 1);
        let replacement_bytes =
            serde_json::to_vec(&replacement_root).expect("serialize replacement root");
        write_owner_only(&replacement_source, &replacement_bytes);
        assert!(
            handle_cli_skills_with_trust_args(
                Some("trust".to_string()),
                Some("provision-remote-root".to_string()),
                None,
                Some(authority_dir.clone()),
                Some(replacement_source),
                Some(sha256_hex(&replacement_bytes)),
                false,
                false,
                false,
            )
            .await
            .is_err()
        );
        assert_eq!(
            std::fs::read(authority_dir.join("remote-trust-root.json"))
                .expect("root remains immutable"),
            root_bytes
        );

        let next_root_signer = hermes_skills::SkillSigningKey::from_seed([33; 32]);
        let next_root = next_root_signer.remote_trust_root("next-root", 2);
        let next_public_key: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(&next_root.root_public_key)
            .expect("decode next root")
            .try_into()
            .expect("Ed25519 public key length");
        let publisher = hermes_skills::SkillSigningKey::from_seed([34; 32]);
        let mut keyring = hermes_skills::SignedRemoteSkillKeyring::new(
            2,
            vec![publisher.trusted_publisher_key(
                "fixture-publisher",
                "fixture-rotated-key",
                true,
            )],
        )
        .expect("create rotated keyring");
        let mut transition = hermes_skills::SkillTrustRootRotation::unsigned(
            "old-root",
            "next-root",
            next_public_key,
            2,
        );
        old_root_signer
            .sign_remote_root_rotation(&mut transition, "old-root")
            .expect("sign old-root transition");
        keyring.root_rotations.push(transition);
        next_root_signer
            .sign_remote_keyring(&mut keyring, "next-root")
            .expect("sign rotated keyring");
        let keyring_source = tmp.path().join("rotated-keyring.json");
        let keyring_bytes = serde_json::to_vec(&keyring).expect("serialize rotated keyring");
        write_owner_only(&keyring_source, &keyring_bytes);
        handle_cli_skills_with_trust_args(
            Some("trust".to_string()),
            Some("rotate-remote-root".to_string()),
            None,
            Some(authority_dir.clone()),
            Some(keyring_source),
            Some(sha256_hex(&keyring_bytes)),
            false,
            false,
            false,
        )
        .await
        .expect("install old-root-authorized rotation");
        assert_eq!(
            std::fs::read(authority_dir.join("remote-trust-root.json"))
                .expect("root stays immutable"),
            root_bytes
        );
        assert_eq!(
            hermes_skills::load_signed_remote_keyring(&skills_root)
                .expect("rotated keyring")
                .epoch,
            2
        );
        hermes_skills::SkillTrustAuthorityPaths::new(
            skills_root,
            authority_dir.join("remote-trust-root.json"),
            authority_dir.join("remote-keyring-audit.json"),
            authority_dir.join("local-owner-keyring.json"),
        )
        .expect("authority paths")
        .build_verifier(
            hermes_skills::SkillTrustPolicy::default(),
            std::collections::BTreeMap::new(),
        )
        .expect("anchored rotation verifier");
    }

    #[tokio::test]
    async fn remote_install_has_no_tofu_network_or_hub_keyring_path() {
        use wiremock::MockServer;

        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home = TempHomeGuard::new(tmp.path());
        let skills_root = tmp.path().join("skills");
        let authority_dir = tmp.path().join("authority");
        handle_cli_skills_with_trust_args(
            Some("trust".to_string()),
            Some("init-local-owner-keyring".to_string()),
            None,
            Some(authority_dir.clone()),
            None,
            None,
            false,
            false,
            false,
        )
        .await
        .expect("initialize local owner keyring");
        std::fs::create_dir_all(skills_root.join(".hub")).expect("create mutable hub");
        write_owner_only(
            &skills_root.join(".hub").join("keyring.json"),
            br#"{"schema_version":1,"publisher_keys":[],"root_rotations":[],"signature":null}"#,
        );

        let server = MockServer::start().await;
        let _api = EnvVarGuard::set("HERMES_SKILL_GITHUB_API_BASE", server.uri());
        let _raw = EnvVarGuard::set("HERMES_SKILL_GITHUB_RAW_BASE", server.uri());
        let _tier = EnvVarGuard::set("HERMES_SKILLS_EXECUTION_TIER", "open");
        assert!(
            handle_cli_skills_with_trust_args(
                Some("install".to_string()),
                Some(format!(
                    "github/fixture/demo/skills/demo@{}",
                    fixture_revision()
                )),
                None,
                Some(authority_dir),
                None,
                None,
                false,
                false,
                false,
            )
            .await
            .is_err()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(!skills_root.join(".packages").exists());
    }

    #[test]
    fn failed_update_verification_preserves_tampered_prior_tree_and_lock() {
        let tmp = tempdir().expect("tempdir");
        let signer = hermes_skills::SkillSigningKey::from_seed([9; 32]);
        let source_v1 = tmp.path().join("source-v1");
        let files_v1 = write_fixture_bundle(&source_v1, &skill_document("rollback", "# Version one\n"));
        let manifest_v1 = signed_remote_manifest(
            &source_v1,
            "rollback",
            "github://fixture/rollback",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let skills_root = tmp.path().join("skills");
        let authority = fixture_authority(&skills_root, &tmp.path().join("authority"), &signer);
        install_verified_skill_material(
            &skills_root,
            SkillInstallMaterial {
                install_name: "rollback".to_string(),
                manifest: manifest_v1.clone(),
                files: files_v1,
            },
            &authority,
            false,
        )
        .expect("initial install");
        let prior_lock = hermes_skills::load_lock(&skills_root).expect("read initial lock");
        let prior_skill = skills_root.join(&prior_lock.packages[0].install_path);
        std::fs::write(prior_skill.join("SKILL.md"), "# Tampered prior\n")
            .expect("tamper prior");

        let source_v2 = tmp.path().join("source-v2");
        let files_v2 = write_fixture_bundle(&source_v2, &skill_document("rollback", "# Version two\n"));
        let mut bad_manifest = signed_remote_manifest(
            &source_v2,
            "rollback",
            "github://fixture/rollback",
            "fedcba9876543210fedcba9876543210fedcba98",
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        if let hermes_skills::SkillPackageProof::RemoteSignature(signature) =
            &mut bad_manifest.proof
        {
            signature.signature = "corrupted".to_string();
        }
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "rollback".to_string(),
                    manifest: bad_manifest,
                    files: files_v2,
                },
                &authority,
                false,
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(prior_skill.join("SKILL.md")).expect("prior tree remains"),
            "# Tampered prior\n"
        );
        let lock = hermes_skills::load_lock(&skills_root).expect("prior lock remains");
        assert_eq!(
            lock.packages[0].manifest.identity.content_digest,
            manifest_v1.identity.content_digest
        );
    }

    #[test]
    fn dangerous_and_invalid_payloads_are_denied_before_cutover() {
        let tmp = tempdir().expect("tempdir");
        let signer = hermes_skills::SkillSigningKey::from_seed([10; 32]);
        let danger = tmp.path().join("danger");
        let dangerous_files =
            write_fixture_bundle(&danger, &skill_document("danger", "# Dangerous\nrm -rf /\n"));
        let dangerous_manifest = signed_remote_manifest(
            &danger,
            "danger",
            "github://fixture/danger",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let skills_root = tmp.path().join("skills");
        let authority = fixture_authority(&skills_root, &tmp.path().join("authority"), &signer);
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "danger".to_string(),
                    manifest: dangerous_manifest,
                    files: dangerous_files,
                },
                &authority,
                false,
            )
            .is_err()
        );
        assert!(hermes_skills::load_lock(&skills_root)
            .expect("read empty lock")
            .packages
            .is_empty());

        let traversal_manifest = signed_remote_manifest(
            &danger,
            "traversal",
            "github://fixture/traversal",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "traversal".to_string(),
                    manifest: traversal_manifest,
                    files: vec![(
                        "../SKILL.md".to_string(),
                        Bytes::from_static(b"# traversal\n"),
                    )],
                },
                &authority,
                false,
            )
            .is_err()
        );

        let binary_manifest = signed_remote_manifest(
            &danger,
            "binary",
            "github://fixture/binary",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "binary".to_string(),
                    manifest: binary_manifest,
                    files: vec![(
                        "SKILL.md".to_string(),
                        Bytes::from_static(&[0xff, 0xfe]),
                    )],
                },
                &authority,
                false,
            )
            .is_err()
        );

        #[cfg(unix)]
        {
            let local = tmp.path().join("symlink-source");
            std::fs::create_dir_all(&local).expect("symlink source");
            std::os::unix::fs::symlink("/etc/passwd", local.join("SKILL.md"))
                .expect("create symlink");
            assert!(collect_local_skill_files(&local).is_err());
        }
        assert!(hermes_skills::load_lock(&skills_root)
            .expect("read empty lock after invalid payloads")
            .packages
            .is_empty());
    }

    #[test]
    fn local_attestation_is_explicit_and_recorded_as_local() {
        let tmp = tempdir().expect("tempdir");
        restrict_owner_only_directory(tmp.path());
        let source = tmp.path().join("local-source");
        let files = write_fixture_bundle(
            &source,
            "---\nname: local-source\nversion: 1.0.0\ndescription: Local authored\n---\n# Local authored\n",
        );
        let revision = hermes_skills::canonical_bundle_digest(&source).expect("local digest");
        let local_signer = hermes_skills::LocalOwnerSigningKey::from_seed([15; 32]);
        let owner_keyring = hermes_skills::LocalOwnerKeyring::new(vec![
            local_signer.trusted_key("fixture-owner", "fixture-local-key", true),
        ])
        .expect("local owner keyring");
        let owner_key_path = tmp.path().join("fixture-owner.key");
        std::fs::write(
            &owner_key_path,
            base64::engine::general_purpose::STANDARD.encode([15_u8; 32]),
        )
        .expect("write owner key");
        let owner_keyring_path = tmp.path().join("fixture-owner-keyring.json");
        std::fs::write(
            &owner_keyring_path,
            serde_json::to_vec(&owner_keyring).expect("serialize owner keyring"),
        )
        .expect("write owner keyring");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for path in [&owner_key_path, &owner_keyring_path] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .expect("restrict owner key material");
            }
        }
        let skills_root = tmp.path().join("skills");
        let manifest = install_local_authored_skill(
            &skills_root,
            "local-source",
            &revision,
            "fixture-owner",
            files,
            &owner_key_path,
            &owner_keyring_path,
            "fixture-local-key",
            false,
        )
        .expect("local install");
        assert_eq!(manifest.source, hermes_skills::SkillPackageSource::LocalAuthored);
        assert!(matches!(
            manifest.proof,
            hermes_skills::SkillPackageProof::LocalOwnerAttestation(_)
        ));
        let lock = hermes_skills::load_lock(&skills_root).expect("local lock");
        assert_eq!(
            lock.packages[0].manifest.source,
            hermes_skills::SkillPackageSource::LocalAuthored
        );
        assert_eq!(
            lock.packages[0].manifest.identity.publisher_id,
            "local:fixture-owner"
        );
        assert_eq!(
            skill_package_trust_label(&lock.packages[0].manifest),
            "local-attested:owner=fixture-owner:key=fixture-local-key"
        );
    }

    #[test]
    fn bootstrap_is_opt_in_and_refused_without_a_policy_sandbox_handoff() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("bootstrap-source");
        let mut files =
            write_fixture_bundle(&source, &skill_document("bootstrap", "# Bootstrap fixture\n"));
        std::fs::create_dir_all(source.join("scripts")).expect("scripts");
        std::fs::write(
            source.join("scripts").join("bootstrap.sh"),
            "#!/bin/sh\ntouch SHOULD_NOT_RUN\n",
        )
        .expect("bootstrap script");
        files.push((
            "scripts/bootstrap.sh".to_string(),
            Bytes::from_static(b"#!/bin/sh\ntouch SHOULD_NOT_RUN\n"),
        ));
        let signer = hermes_skills::SkillSigningKey::from_seed([11; 32]);
        let permissions = hermes_skills::SkillPermissions::default();
        let manifest = signed_remote_manifest(
            &source,
            "bootstrap",
            "github://fixture/bootstrap",
            fixture_revision(),
            &signer,
            permissions,
        );
        let skills_root = tmp.path().join("skills");
        let authority = fixture_authority(&skills_root, &tmp.path().join("authority"), &signer);
        assert!(!bootstrap_requested(None));
        install_verified_skill_material(
            &skills_root,
            SkillInstallMaterial {
                install_name: "bootstrap".to_string(),
                manifest: manifest.clone(),
                files: files.clone(),
            },
            &authority,
            false,
        )
        .expect("default installation without bootstrap");
        assert!(!skills_root.join("SHOULD_NOT_RUN").exists());
        assert!(
            install_verified_skill_material(
                &skills_root,
                SkillInstallMaterial {
                    install_name: "bootstrap".to_string(),
                    manifest,
                    files,
                },
                &authority,
                true,
            )
            .is_err()
        );
        assert_eq!(
            hermes_skills::load_lock(&skills_root)
                .expect("retained lock")
                .packages
                .len(),
            1
        );
        assert!(!skills_root.join("SHOULD_NOT_RUN").exists());
    }

    #[tokio::test]
    async fn enforced_audit_quarantines_a_revoked_signed_package() {
        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home = TempHomeGuard::new(tmp.path());
        let source = tmp.path().join("revoked-source");
        let files = write_fixture_bundle(&source, &skill_document("revoked", "# Revoked fixture\n"));
        let signer = hermes_skills::SkillSigningKey::from_seed([12; 32]);
        let manifest = signed_remote_manifest(
            &source,
            "revoked",
            "github://fixture/revoked",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let skills_root = tmp.path().join("skills");
        let authority_dir = tmp.path().join("authority");
        let authority = fixture_authority(&skills_root, &authority_dir, &signer);
        install_verified_skill_material(
            &skills_root,
            SkillInstallMaterial {
                install_name: "revoked".to_string(),
                manifest: manifest.clone(),
                files,
            },
            &authority,
            false,
        )
        .expect("initial install");

        let mut revocations = hermes_skills::SignedSkillRevocationList::empty();
        revocations.issued_at = "2026-07-20T00:00:00Z".to_string();
        revocations.revoked.push(hermes_skills::SkillRevocation {
            name: Some(manifest.identity.name.clone()),
            publisher_id: Some(manifest.identity.publisher_id.clone()),
            immutable_source_revision: Some(manifest.identity.immutable_source_revision.clone()),
            content_digest: Some(manifest.identity.content_digest.clone()),
            key_id: None,
            reason: "fixture revocation".to_string(),
            revoked_at: "2026-07-20T00:00:00Z".to_string(),
        });
        signer
            .sign_revocations(&mut revocations, "fixture-key")
            .expect("sign revocations");
        hermes_skills::store_revocations(&skills_root, &revocations).expect("store revocations");

        let result = handle_cli_skills_with_trust_args(
            Some("audit".to_string()),
            None,
            Some("--enforce".to_string()),
            Some(authority_dir),
            None,
            None,
            false,
            false,
            false,
        )
        .await;
        assert!(result.is_err());
        let lock = hermes_skills::load_lock(&skills_root).expect("quarantined lock");
        assert_eq!(
            lock.packages[0]
                .quarantine
                .as_ref()
                .expect("quarantine")
                .reason,
            hermes_skills::SkillQuarantineReason::RevokedPackage
        );
    }

    #[test]
    fn uninstall_removes_the_v2_lock_entry_before_best_effort_cleanup() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("uninstall-source");
        let files =
            write_fixture_bundle(&source, &skill_document("uninstall", "# Uninstall fixture\n"));
        let signer = hermes_skills::SkillSigningKey::from_seed([13; 32]);
        let manifest = signed_remote_manifest(
            &source,
            "uninstall",
            "github://fixture/uninstall",
            fixture_revision(),
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let skills_root = tmp.path().join("skills");
        let authority = fixture_authority(&skills_root, &tmp.path().join("authority"), &signer);
        install_verified_skill_material(
            &skills_root,
            SkillInstallMaterial {
                install_name: "uninstall".to_string(),
                manifest,
                files,
            },
            &authority,
            false,
        )
        .expect("install");
        let installed_path = hermes_skills::load_lock(&skills_root)
            .expect("lock")
            .packages[0]
            .install_path
            .clone();

        let result = uninstall_verified_skill(&skills_root, "uninstall").expect("uninstall");
        assert!(result.removed.is_some());
        assert!(result.cleanup_error.is_none());
        assert!(hermes_skills::load_lock(&skills_root)
            .expect("lock")
            .packages
            .is_empty());
        assert!(!skills_root.join(installed_path).exists());
    }

    #[tokio::test]
    async fn tampered_update_download_from_local_http_fixture_rolls_back() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _guard = env_test_lock();
        let tmp = tempdir().expect("tempdir");
        let _home = TempHomeGuard::new(tmp.path());
        let signer = hermes_skills::SkillSigningKey::from_seed([14; 32]);
        let skills_root = tmp.path().join("skills");
        let authority_dir = tmp.path().join("authority");
        let authority = fixture_authority(&skills_root, &authority_dir, &signer);

        let source_v1 = tmp.path().join("update-v1");
        let files_v1 = write_fixture_bundle(&source_v1, &skill_document("update", "# Prior package\n"));
        let prior_manifest = signed_remote_manifest(
            &source_v1,
            "update",
            "github://fixture/update/skills/update",
            "1111111111111111111111111111111111111111",
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        install_verified_skill_material(
            &skills_root,
            SkillInstallMaterial {
                install_name: "update".to_string(),
                manifest: prior_manifest.clone(),
                files: files_v1,
            },
            &authority,
            false,
        )
        .expect("prior install");
        let prior_path = skills_root.join(
            hermes_skills::load_lock(&skills_root)
                .expect("prior lock")
                .packages[0]
                .install_path
                .clone(),
        );

        let source_v2 = tmp.path().join("update-v2");
        let _ = write_fixture_bundle(&source_v2, &skill_document("update", "# Signed update payload\n"));
        let update_revision = "2222222222222222222222222222222222222222";
        let update_manifest = signed_remote_manifest(
            &source_v2,
            "update",
            "github://fixture/update/skills/update",
            update_revision,
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let manifest_path = tmp.path().join("update-manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&update_manifest).expect("serialize manifest"),
        )
        .expect("write manifest");

        let foreign_manifest = signed_remote_manifest(
            &source_v2,
            "update",
            "github://fixture/foreign/skills/update",
            update_revision,
            &signer,
            hermes_skills::SkillPermissions::default(),
        );
        let foreign_manifest_path = tmp.path().join("foreign-update-manifest.json");
        std::fs::write(
            &foreign_manifest_path,
            serde_json::to_vec(&foreign_manifest).expect("serialize foreign manifest"),
        )
        .expect("write foreign manifest");

        let server = MockServer::start().await;
        let _api = EnvVarGuard::set("HERMES_SKILL_GITHUB_API_BASE", server.uri());
        let _raw = EnvVarGuard::set("HERMES_SKILL_GITHUB_RAW_BASE", server.uri());
        let foreign_result = handle_cli_skills_with_trust_args(
            Some("update".to_string()),
            None,
            Some(format!(
                "--apply --manifest {}",
                foreign_manifest_path.display()
            )),
            Some(authority_dir.clone()),
            None,
            None,
            false,
            false,
            false,
        )
        .await;
        assert!(
            foreign_result
                .expect_err("foreign source update must be refused")
                .to_string()
                .contains("provenance switch")
        );
        assert!(server.received_requests().await.unwrap().is_empty());

        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/fixture/update/commits/{update_revision}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sha": update_revision
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/fixture/update/git/trees/{update_revision}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tree": [{
                    "path": "skills/update/SKILL.md",
                    "type": "blob"
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/fixture/update/{update_revision}/skills/update/SKILL.md"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string("# Tampered download\n"))
            .mount(&server)
            .await;

        assert!(
            handle_cli_skills_with_trust_args(
                Some("update".to_string()),
                None,
                Some(format!("--apply --manifest {}", manifest_path.display())),
                Some(authority_dir),
                None,
                None,
                false,
                false,
                false,
            )
            .await
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(prior_path.join("SKILL.md")).expect("prior skill remains"),
            skill_document("update", "# Prior package\n")
        );
        let lock = hermes_skills::load_lock(&skills_root).expect("prior lock");
        assert_eq!(
            lock.packages[0].manifest.identity.content_digest,
            prior_manifest.identity.content_digest
        );
    }
