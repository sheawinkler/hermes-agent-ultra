#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_config_default() {
        let cfg = GatewayConfig::default();
        assert_eq!(cfg.max_turns, 250);
        assert!(!cfg.tools.is_empty());
        assert!(cfg.model.is_none());
        assert_eq!(cfg.tool_output, ToolOutputConfig::default());
        assert!(cfg.auxiliary.is_empty());
        assert!(cfg.delegation.max_spawn_depth.is_none());
        assert!(cfg.tts.is_null());
        assert!(cfg.proxy.is_none());
        assert_eq!(
            cfg.platform_toolsets
                .get("cli")
                .cloned()
                .unwrap_or_default(),
            vec!["hermes-cli".to_string()]
        );
        assert_eq!(
            cfg.platform_toolsets
                .get("telegram")
                .cloned()
                .unwrap_or_default(),
            vec!["hermes-telegram".to_string()]
        );
        assert_eq!(
            cfg.platform_toolsets
                .get("cron")
                .cloned()
                .unwrap_or_default(),
            vec!["hermes-cron".to_string()]
        );
    }

    #[test]
    fn gateway_config_serde_roundtrip() {
        let mut cfg = GatewayConfig::default();
        cfg.auxiliary.insert(
            "vision".to_string(),
            AuxiliaryTaskConfig {
                provider: "openrouter".to_string(),
                model: "google/gemini-2.5-flash".to_string(),
                ..Default::default()
            },
        );
        cfg.tts = serde_json::json!({
            "provider": "piper",
            "piper": {"voice": "en_US-lessac-medium"}
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_turns, cfg.max_turns);
        assert_eq!(back.tools, cfg.tools);
        assert_eq!(back.auxiliary["vision"].model, "google/gemini-2.5-flash");
        assert_eq!(back.tts["provider"], "piper");
    }

    #[test]
    fn delegation_config_accepts_uncapped_max_spawn_depth() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
delegation:
  max_spawn_depth: 99
"#,
        )
        .expect("delegation config should deserialize");

        assert_eq!(cfg.delegation.max_spawn_depth, Some(99));

        let json = serde_json::to_string(&cfg).unwrap();
        let back: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.delegation.max_spawn_depth, Some(99));
    }

    #[test]
    fn delegation_config_accepts_provider_model_and_direct_endpoint() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
delegation:
  model: google/gemini-3-flash-preview
  provider: openrouter
  base_url: http://localhost:1234/v1
  api_key: local-key
"#,
        )
        .expect("delegation provider/model config should deserialize");

        assert_eq!(
            cfg.delegation.model.as_deref(),
            Some("google/gemini-3-flash-preview")
        );
        assert_eq!(cfg.delegation.provider.as_deref(), Some("openrouter"));
        assert_eq!(
            cfg.delegation.base_url.as_deref(),
            Some("http://localhost:1234/v1")
        );
        assert_eq!(cfg.delegation.api_key.as_deref(), Some("local-key"));

        let json = serde_json::to_string(&cfg).unwrap();
        let back: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.delegation, cfg.delegation);
    }

    #[test]
    fn llm_provider_config_accepts_request_timeout_seconds() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
llm_providers:
  anthropic:
    api_key_env: ANTHROPIC_API_KEY
    request_timeout_seconds: 45.5
"#,
        )
        .expect("provider config should deserialize");

        assert_eq!(
            cfg.llm_providers
                .get("anthropic")
                .and_then(|provider| provider.request_timeout_seconds),
            Some(45.5)
        );

        let yaml = serde_yaml::to_string(&cfg).expect("serialize config");
        assert!(yaml.contains("request_timeout_seconds: 45.5"));
    }

    #[test]
    fn default_auxiliary_task_configs_match_upstream_shape() {
        let tasks = default_auxiliary_task_configs();
        for key in ["vision", "web_extract", "approval"] {
            let task = tasks.get(key).expect("built-in task default");
            assert_eq!(task.provider, "auto");
            assert_eq!(task.model, "");
            assert_eq!(task.base_url, "");
            assert_eq!(task.api_key, "");
        }
        assert_eq!(tasks["vision"].timeout, Some(120));
        assert_eq!(tasks["vision"].download_timeout, Some(30));
        assert_eq!(tasks["web_extract"].timeout, Some(360));
        assert_eq!(tasks["curator"].timeout, Some(600));
    }

    #[test]
    fn builtin_auxiliary_env_overrides_bridge_non_default_values() {
        let mut cfg = GatewayConfig::default();
        cfg.auxiliary.insert(
            "vision".to_string(),
            AuxiliaryTaskConfig {
                provider: "  openrouter  ".to_string(),
                model: "  google/gemini-2.5-flash  ".to_string(),
                ..Default::default()
            },
        );
        cfg.auxiliary.insert(
            "web_extract".to_string(),
            AuxiliaryTaskConfig {
                provider: "auto".to_string(),
                model: "custom-llm".to_string(),
                ..Default::default()
            },
        );
        cfg.auxiliary.insert(
            "approval".to_string(),
            AuxiliaryTaskConfig {
                base_url: "http://localhost:1234/v1".to_string(),
                api_key: "local-key".to_string(),
                ..Default::default()
            },
        );

        assert_eq!(
            cfg.builtin_auxiliary_env_overrides(),
            vec![
                (
                    "AUXILIARY_APPROVAL_BASE_URL".to_string(),
                    "http://localhost:1234/v1".to_string()
                ),
                (
                    "AUXILIARY_APPROVAL_API_KEY".to_string(),
                    "local-key".to_string()
                ),
                (
                    "AUXILIARY_VISION_PROVIDER".to_string(),
                    "openrouter".to_string()
                ),
                (
                    "AUXILIARY_VISION_MODEL".to_string(),
                    "google/gemini-2.5-flash".to_string()
                ),
                (
                    "AUXILIARY_WEB_EXTRACT_MODEL".to_string(),
                    "custom-llm".to_string()
                ),
            ]
        );
    }

    #[test]
    fn auxiliary_env_overrides_skip_compression_until_registered() {
        let mut cfg = GatewayConfig::default();
        cfg.auxiliary.insert(
            "compression".to_string(),
            AuxiliaryTaskConfig {
                provider: "openrouter".to_string(),
                model: "compressor".to_string(),
                ..Default::default()
            },
        );

        assert!(cfg.builtin_auxiliary_env_overrides().is_empty());
        assert_eq!(
            cfg.auxiliary_env_overrides_for(["compression"]),
            vec![
                (
                    "AUXILIARY_COMPRESSION_PROVIDER".to_string(),
                    "openrouter".to_string()
                ),
                (
                    "AUXILIARY_COMPRESSION_MODEL".to_string(),
                    "compressor".to_string()
                ),
            ]
        );
    }

    #[test]
    fn stale_auxiliary_assignments_report_provider_mismatches() {
        let mut cfg = GatewayConfig::default();
        cfg.auxiliary.insert(
            "compression".to_string(),
            AuxiliaryTaskConfig {
                provider: "nous".to_string(),
                model: "hermes-4".to_string(),
                ..Default::default()
            },
        );
        cfg.auxiliary.insert(
            "vision".to_string(),
            AuxiliaryTaskConfig {
                provider: "auto".to_string(),
                model: "ignored".to_string(),
                ..Default::default()
            },
        );
        cfg.auxiliary.insert(
            "curator".to_string(),
            AuxiliaryTaskConfig {
                provider: "openrouter".to_string(),
                model: "anthropic/claude-opus-4.7".to_string(),
                ..Default::default()
            },
        );

        let stale = cfg.stale_auxiliary_assignments_for_main_provider("openrouter");
        assert_eq!(
            stale,
            vec![StaleAuxiliaryAssignment {
                task: "compression".to_string(),
                provider: "nous".to_string(),
                model: "hermes-4".to_string(),
            }]
        );
        assert!(cfg
            .stale_auxiliary_assignments_for_main_provider("nous")
            .iter()
            .any(|entry| entry.task == "curator"));
    }

    #[test]
    fn config_null_string_guards_match_python_tool_defaults() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
web:
  backend: null
  search_backend: null
  extract_backend: null
  crawl_backend: null
auxiliary:
  compression:
    provider: null
    model: null
    base_url: null
    api_key: null
tts:
  provider: null
mcp_servers:
  - name: local
    command: hermes-mcp
    auth: null
    keepalive_interval: 10
"#,
        )
        .expect("null-valued config fields should deserialize");

        assert_eq!(cfg.web, WebConfig::default());
        let compression = cfg.auxiliary.get("compression").expect("compression task");
        assert_eq!(compression.provider, "auto");
        assert_eq!(compression.model, "");
        assert_eq!(compression.base_url, "");
        assert_eq!(compression.api_key, "");
        assert_eq!(cfg.tts["provider"], serde_json::Value::Null);
        assert_eq!(cfg.mcp_servers.len(), 1);
        assert_eq!(cfg.mcp_servers[0].name, "local");
        assert_eq!(cfg.mcp_servers[0].keepalive_interval, Some(10));
    }

    #[test]
    fn config_null_guards_preserve_valid_strings() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
web:
  backend: tavily
  search_backend: brave-free
  extract_backend: firecrawl
  crawl_backend: tavily
auxiliary:
  vision:
    provider: OPENROUTER
    model: google/gemini-2.5-flash
    base_url: https://router.example/v1
    api_key: local-key
"#,
        )
        .expect("valid string-valued config fields should deserialize");

        assert_eq!(cfg.web.backend, "tavily");
        assert_eq!(cfg.web.search_backend, "brave-free");
        assert_eq!(cfg.web.extract_backend, "firecrawl");
        assert_eq!(cfg.web.crawl_backend, "tavily");
        let vision = cfg.auxiliary.get("vision").expect("vision task");
        assert_eq!(vision.provider, "OPENROUTER");
        assert_eq!(vision.model, "google/gemini-2.5-flash");
        assert_eq!(vision.base_url, "https://router.example/v1");
        assert_eq!(vision.api_key, "local-key");
    }

    #[test]
    fn quick_commands_deserialize_exec_and_alias_configs() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
quick_commands:
  dn:
    type: exec
    command: echo daily-note
    timeout_secs: 5
  sc:
    type: alias
    target: /context
"#,
        )
        .expect("quick command config");

        let exec = cfg.quick_commands.get("dn").expect("exec command");
        assert_eq!(exec.kind, "exec");
        assert_eq!(exec.command.as_deref(), Some("echo daily-note"));
        assert_eq!(exec.timeout_secs(), 5);

        let alias = cfg.quick_commands.get("sc").expect("alias command");
        assert_eq!(alias.kind, "alias");
        assert_eq!(alias.target.as_deref(), Some("/context"));
    }

    #[test]
    fn display_config_accepts_boolish_verbose_gate_and_platform_modes() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
display:
  tool_progress_command: "true"
  tool_progress: all
  busy_input_mode: steer
  busy_ack_enabled: "false"
  memory_notifications: "false"
  platforms:
    telegram:
      tool_progress: off
agent:
  service_tier: fast
  preflight_context_compress: "false"
"#,
        )
        .expect("display config");

        assert!(cfg.display.tool_progress_command_enabled());
        assert_eq!(cfg.display.platform_tool_progress("telegram"), Some("off"));
        assert_eq!(cfg.display.platform_tool_progress("slack"), Some("all"));
        assert_eq!(cfg.display.normalized_busy_input_mode(), "steer");
        assert!(!cfg.display.busy_ack_enabled());
        assert!(!cfg.display.memory_notifications_enabled());
        assert_eq!(
            cfg.agent.normalized_service_tier().as_deref(),
            Some("priority")
        );
        assert!(!cfg.agent.preflight_context_compress);

        let disabled: GatewayConfig = serde_yaml::from_str(
            r#"
display:
  tool_progress_command: "false"
"#,
        )
        .expect("quoted false");
        assert!(!disabled.display.tool_progress_command_enabled());
    }

    #[test]
    fn tool_output_config_default_matches_upstream_limits() {
        let tool_output = ToolOutputConfig::default();
        assert_eq!(tool_output.max_bytes, DEFAULT_TOOL_OUTPUT_MAX_BYTES);
        assert_eq!(tool_output.max_lines, DEFAULT_TOOL_OUTPUT_MAX_LINES);
        assert_eq!(
            tool_output.max_line_length,
            DEFAULT_TOOL_OUTPUT_MAX_LINE_LENGTH
        );
    }

    #[test]
    fn tool_output_config_accepts_partial_positive_overrides() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
tool_output:
  max_bytes: "75000"
  max_lines: 50
"#,
        )
        .expect("tool_output config");

        assert_eq!(cfg.tool_output.max_bytes, 75_000);
        assert_eq!(cfg.tool_output.max_lines, 50);
        assert_eq!(
            cfg.tool_output.max_line_length,
            DEFAULT_TOOL_OUTPUT_MAX_LINE_LENGTH
        );
    }

    #[test]
    fn tool_output_config_rejects_invalid_values_to_field_defaults() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
tool_output:
  max_bytes: null
  max_lines: -1
  max_line_length: 0
"#,
        )
        .expect("tool_output fallback config");

        assert_eq!(cfg.tool_output, ToolOutputConfig::default());
    }

    #[test]
    fn tool_output_config_non_object_falls_back_to_defaults() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
tool_output: nonsense
"#,
        )
        .expect("non-object tool_output fallback");

        assert_eq!(cfg.tool_output, ToolOutputConfig::default());
    }

    #[test]
    fn terminal_backend_type_serde() {
        let t = TerminalBackendType::Docker;
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"docker\"");
        let back: TerminalBackendType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TerminalBackendType::Docker);
    }

    #[test]
    fn terminal_config_accepts_env_passthrough_list() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
terminal:
  env_passthrough:
    - OPENAI_API_KEY
    - TENOR_API_KEY
"#,
        )
        .expect("terminal env passthrough config");

        assert_eq!(
            cfg.terminal.env_passthrough,
            vec!["OPENAI_API_KEY".to_string(), "TENOR_API_KEY".to_string()]
        );
    }

    #[test]
    fn terminal_config_accepts_home_mode() {
        let cfg: GatewayConfig = serde_yaml::from_str(
            r#"
terminal:
  home_mode: profile
"#,
        )
        .expect("terminal home mode config");

        assert_eq!(cfg.terminal.home_mode, TerminalHomeMode::Profile);
        assert_eq!(
            TerminalHomeMode::from_env_name("real"),
            Some(TerminalHomeMode::Real)
        );
    }

    #[test]
    fn approval_config_default() {
        let a = ApprovalConfig::default();
        assert!(!a.enabled);
        assert!(!a.require_approval);
        assert!(a.dangerous_commands.is_empty());
    }

    #[test]
    fn security_config_default() {
        let s = SecurityConfig::default();
        assert!(!s.allow_private_urls);
        assert!(!s.website_blocklist.enabled);
        assert!(s.website_blocklist.domains.is_empty());
        assert!(s.website_blocklist.shared_files.is_empty());
    }

    #[test]
    fn web_config_default_matches_upstream_empty_selectors() {
        let web = WebConfig::default();
        assert_eq!(web.backend, "");
        assert_eq!(web.search_backend, "");
        assert_eq!(web.extract_backend, "");
        assert_eq!(web.crawl_backend, "");
    }

    #[test]
    fn proxy_config_serde() {
        let p = ProxyConfig {
            http_proxy: Some("http://proxy:8080".into()),
            socks_proxy: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.http_proxy, Some("http://proxy:8080".to_string()));
        assert_eq!(back.socks_proxy, None);
    }
}
