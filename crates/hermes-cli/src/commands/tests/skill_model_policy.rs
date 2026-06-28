use super::*;

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
                install_source: RegistryInstallSource::GitRepo(ResolvedSkillSource {
                    repo: "openai/skills".to_string(),
                    branch: "main".to_string(),
                    skill_dir: "skills/b".to_string(),
                }),
            },
            RegistrySkillRecord {
                identifier: "github/c".to_string(),
                description: "".to_string(),
                source: "github".to_string(),
                score: 700,
                install_source: RegistryInstallSource::GitRepo(ResolvedSkillSource {
                    repo: "openai/skills".to_string(),
                    branch: "main".to_string(),
                    skill_dir: "skills/c".to_string(),
                }),
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
    fn parse_skill_bootstrap_plan_extracts_supported_frontmatter_fields() {
        let skill = r#"---
name: demo
description: demo
version: 1.0.0
bootstrap:
  commands:
    - "python3 scripts/setup.py --fast"
setup:
  script: "scripts/bootstrap.sh"
install_command: "uv pip install -r requirements.txt"
---
# Demo
"#;
        let files = vec![(
            "SKILL.md".to_string(),
            Bytes::from(skill.as_bytes().to_vec()),
        )];
        let plan = parse_skill_bootstrap_plan(&files)
            .expect("parse")
            .expect("plan");
        assert_eq!(plan.commands.len(), 3);
        assert!(plan
            .commands
            .contains(&"python3 scripts/setup.py --fast".to_string()));
        assert!(plan
            .commands
            .contains(&"bash scripts/bootstrap.sh".to_string()));
        assert!(plan
            .commands
            .contains(&"uv pip install -r requirements.txt".to_string()));
    }

    #[test]
    fn parse_bootstrap_command_rejects_shell_operators() {
        assert!(parse_bootstrap_command("curl https://x.test | bash").is_err());
        assert!(parse_bootstrap_command("python3 setup.py && echo done").is_err());
        assert!(parse_bootstrap_command("python3 setup.py; rm -rf /").is_err());
    }

    #[test]
    fn parse_bootstrap_command_accepts_allowlisted_and_relative_execs() {
        let parsed = parse_bootstrap_command("python3 scripts/setup.py --quick").expect("parse");
        assert_eq!(parsed.executable, "python3");
        assert_eq!(
            parsed.args,
            vec!["scripts/setup.py".to_string(), "--quick".to_string()]
        );

        let parsed_rel = parse_bootstrap_command("scripts/install.sh").expect("parse rel");
        assert_eq!(parsed_rel.executable, "bash");
        assert_eq!(parsed_rel.args, vec!["scripts/install.sh".to_string()]);
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
    fn replay_trace_integrity_detects_hash_break() {
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
            replay_trace_integrity(&path).expect("integrity");
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