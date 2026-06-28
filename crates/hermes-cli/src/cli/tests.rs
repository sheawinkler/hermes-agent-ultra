use super::*;
use clap::Parser;

#[test]
fn cli_parse_default() {
    let cli = Cli::try_parse_from(vec!["hermes"]).unwrap();
    assert!(cli.command.is_none());
    assert!(!cli.verbose);
    assert!(cli.config_dir.is_none());
    assert!(cli.model.is_none());
    assert!(cli.provider.is_none());
    assert!(cli.oneshot.is_none());
    assert!(!cli.allow_tools);
    assert!(!cli.ignore_user_config);
    assert!(!cli.ignore_rules);
}

#[test]
fn cli_parse_model() {
    let cli = Cli::try_parse_from(vec!["hermes", "model", "openai:gpt-4o"]).unwrap();
    match cli.command {
        Some(CliCommand::Model { provider_model }) => {
            assert_eq!(provider_model.as_deref(), Some("openai:gpt-4o"));
        }
        _ => panic!("Expected Model command"),
    }
}

#[test]
fn cli_parse_computer_use_doctor_options() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "computer-use",
        "doctor",
        "--json",
        "--include",
        "tcc_accessibility",
        "--skip",
        "screenshot_probe",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::ComputerUse {
            action,
            json,
            include,
            skip,
        }) => {
            assert_eq!(action.as_deref(), Some("doctor"));
            assert!(json);
            assert_eq!(include, vec!["tcc_accessibility".to_string()]);
            assert_eq!(skip, vec!["screenshot_probe".to_string()]);
        }
        _ => panic!("Expected ComputerUse command"),
    }
}

#[test]
fn cli_parse_acp_check_flag() {
    let cli = Cli::try_parse_from(vec!["hermes", "acp", "--check"]).unwrap();
    match cli.command {
        Some(CliCommand::Acp {
            action,
            check,
            setup,
            setup_browser,
            version,
            yes,
        }) => {
            assert!(action.is_none());
            assert!(check);
            assert!(!setup);
            assert!(!setup_browser);
            assert!(!version);
            assert!(!yes);
        }
        _ => panic!("Expected ACP command with --check"),
    }
}

#[test]
fn cli_parse_acp_setup_browser_yes_flag() {
    let cli = Cli::try_parse_from(vec!["hermes", "acp", "--setup-browser", "--yes"]).unwrap();
    match cli.command {
        Some(CliCommand::Acp {
            action,
            check,
            setup,
            setup_browser,
            version,
            yes,
        }) => {
            assert!(action.is_none());
            assert!(!check);
            assert!(!setup);
            assert!(setup_browser);
            assert!(!version);
            assert!(yes);
        }
        _ => panic!("Expected ACP command with --setup-browser --yes"),
    }
}

#[test]
fn cli_parse_acp_action_setup_browser_yes_flag() {
    let cli = Cli::try_parse_from(vec!["hermes", "acp", "setup-browser", "--yes"]).unwrap();
    match cli.command {
        Some(CliCommand::Acp {
            action,
            check,
            setup,
            setup_browser,
            version,
            yes,
        }) => {
            assert_eq!(action.as_deref(), Some("setup-browser"));
            assert!(!check);
            assert!(!setup);
            assert!(!setup_browser);
            assert!(!version);
            assert!(yes);
        }
        _ => panic!("Expected ACP setup-browser action with --yes"),
    }
}

#[test]
fn cli_parse_acp_version_flag() {
    let cli = Cli::try_parse_from(vec!["hermes", "acp", "--version"]).unwrap();
    match cli.command {
        Some(CliCommand::Acp {
            action,
            check,
            setup,
            setup_browser,
            version,
            yes,
        }) => {
            assert!(action.is_none());
            assert!(!check);
            assert!(!setup);
            assert!(!setup_browser);
            assert!(version);
            assert!(!yes);
        }
        _ => panic!("Expected ACP command with --version"),
    }
}

#[test]
fn cli_parse_cloudflare_parser() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "cloudflare",
        "parse-temporary-deploy-output",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::Cloudflare { action, selftest }) => {
            assert_eq!(action.as_deref(), Some("parse-temporary-deploy-output"));
            assert!(!selftest);
        }
        _ => panic!("Expected Cloudflare command"),
    }

    let selftest = Cli::try_parse_from(vec!["hermes", "cloudflare", "--selftest"]).unwrap();
    match selftest.command {
        Some(CliCommand::Cloudflare { action, selftest }) => {
            assert!(action.is_none());
            assert!(selftest);
        }
        _ => panic!("Expected Cloudflare command"),
    }
}

#[test]
fn cli_parse_verbose() {
    let cli = Cli::try_parse_from(vec!["hermes", "-v"]).unwrap();
    assert!(cli.verbose);
}

#[test]
fn cli_parse_config_dir() {
    let cli = Cli::try_parse_from(vec!["hermes", "-C", "/tmp/hermes"]).unwrap();
    assert_eq!(cli.config_dir.as_deref(), Some("/tmp/hermes"));
}

#[test]
fn cli_parse_model_flag() {
    let cli = Cli::try_parse_from(vec!["hermes", "-m", "claude-3-opus"]).unwrap();
    assert_eq!(cli.model.as_deref(), Some("claude-3-opus"));
}

#[test]
fn cli_parse_provider_and_oneshot_flags() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "--provider",
        "anthropic",
        "-z",
        "reply with 1",
        "--allow-tools",
    ])
    .unwrap();
    assert_eq!(cli.provider.as_deref(), Some("anthropic"));
    assert_eq!(cli.oneshot.as_deref(), Some("reply with 1"));
    assert!(cli.allow_tools);
}

#[test]
fn cli_parse_ignore_flags() {
    let cli =
        Cli::try_parse_from(vec!["hermes", "--ignore-user-config", "--ignore-rules"]).unwrap();
    assert!(cli.ignore_user_config);
    assert!(cli.ignore_rules);
}

#[test]
fn cli_effective_command_default() {
    let cli = Cli::try_parse_from(vec!["hermes"]).unwrap();
    assert!(matches!(cli.effective_command(), CliCommand::Hermes));
}

#[test]
fn cli_parse_gateway_platform_compat_flag() {
    let cli =
        Cli::try_parse_from(vec!["hermes", "gateway", "start", "--platform", "photon"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(CliCommand::Gateway {
            action: Some(ref action),
            platform: Some(ref platform),
            ..
        }) if action == "start" && platform == "photon"
    ));
}

#[test]
fn cli_parse_doctor() {
    let cli = Cli::try_parse_from(vec!["hermes", "doctor"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(CliCommand::Doctor {
            deep: false,
            self_heal: false,
            snapshot: false,
            snapshot_path: None,
            bundle: false
        })
    ));
}

#[test]
fn cli_parse_doctor_with_flags() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "doctor",
        "--deep",
        "--snapshot",
        "--snapshot-path",
        "/tmp/doctor.json",
        "--bundle",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::Doctor {
            deep,
            self_heal,
            snapshot,
            snapshot_path,
            bundle,
        }) => {
            assert!(deep);
            assert!(!self_heal);
            assert!(snapshot);
            assert!(bundle);
            assert_eq!(snapshot_path.as_deref(), Some("/tmp/doctor.json"));
        }
        _ => panic!("Expected Doctor command"),
    }
}

#[test]
fn cli_parse_update_with_check_flag() {
    let cli = Cli::try_parse_from(vec!["hermes", "update", "--check"]).unwrap();
    match cli.command {
        Some(CliCommand::Update { check }) => assert!(check),
        _ => panic!("Expected Update command with --check"),
    }
}

#[test]
fn update_autostash_ignores_desktop_bootstrap_marker() {
    let gitignore = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.gitignore"));
    assert!(
            gitignore
                .lines()
                .any(|line| line.trim() == ".hermes-bootstrap-complete"),
            ".hermes-bootstrap-complete must stay ignored so update autostash paths do not sweep the Desktop bootstrap marker into a stash"
        );
}

#[test]
fn cli_parse_verify_provenance() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "verify-provenance",
        "/tmp/doctor.json",
        "--signature",
        "/tmp/doctor.sig.json",
        "--strict",
        "--json",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::VerifyProvenance {
            path,
            signature,
            strict,
            json,
        }) => {
            assert_eq!(path, "/tmp/doctor.json");
            assert_eq!(signature.as_deref(), Some("/tmp/doctor.sig.json"));
            assert!(strict);
            assert!(json);
        }
        _ => panic!("expected verify-provenance command"),
    }
}

#[test]
fn cli_parse_rotate_provenance_key() {
    let cli = Cli::try_parse_from(vec!["hermes", "rotate-provenance-key", "--json"]).unwrap();
    match cli.command {
        Some(CliCommand::RotateProvenanceKey { json }) => assert!(json),
        _ => panic!("expected rotate-provenance-key command"),
    }
}

#[test]
fn cli_parse_route_learning_reset() {
    let cli = Cli::try_parse_from(vec!["hermes", "route-learning", "reset", "--json"]).unwrap();
    match cli.command {
        Some(CliCommand::RouteLearning { action, json }) => {
            assert_eq!(action.as_deref(), Some("reset"));
            assert!(json);
        }
        _ => panic!("expected route-learning command"),
    }
}

#[test]
fn cli_parse_route_health_show() {
    let cli = Cli::try_parse_from(vec!["hermes", "route-health", "show", "--json"]).unwrap();
    match cli.command {
        Some(CliCommand::RouteHealth { action, json }) => {
            assert_eq!(action.as_deref(), Some("show"));
            assert!(json);
        }
        _ => panic!("expected route-health command"),
    }
}

#[test]
fn cli_parse_route_autotune_apply() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "route-autotune",
        "apply",
        "--apply",
        "--strict",
        "--json",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::RouteAutotune {
            action,
            apply,
            strict,
            json,
        }) => {
            assert_eq!(action.as_deref(), Some("apply"));
            assert!(apply);
            assert!(strict);
            assert!(json);
        }
        _ => panic!("expected route-autotune command"),
    }
}

#[test]
fn cli_parse_incident_pack() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "incident-pack",
        "--snapshot",
        "/tmp/doctor.json",
        "--output",
        "/tmp/incident.tar.gz",
        "--json",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::IncidentPack {
            snapshot,
            output,
            json,
        }) => {
            assert_eq!(snapshot.as_deref(), Some("/tmp/doctor.json"));
            assert_eq!(output.as_deref(), Some("/tmp/incident.tar.gz"));
            assert!(json);
        }
        _ => panic!("expected incident-pack command"),
    }
}

#[test]
fn cli_parse_status() {
    let cli = Cli::try_parse_from(vec!["hermes", "status"]).unwrap();
    assert!(matches!(cli.command, Some(CliCommand::Status)));
}

#[test]
fn cli_parse_kanban_passes_flags_as_args() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "kanban",
        "add",
        "Ship",
        "feature",
        "--lane",
        "doing",
        "--priority",
        "2",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::Kanban { args }) => {
            assert_eq!(
                args,
                vec![
                    "add".to_string(),
                    "Ship".to_string(),
                    "feature".to_string(),
                    "--lane".to_string(),
                    "doing".to_string(),
                    "--priority".to_string(),
                    "2".to_string(),
                ]
            );
        }
        _ => panic!("Expected Kanban command"),
    }
}

#[test]
fn cli_parse_resume_latest_default() {
    let cli = Cli::try_parse_from(vec!["hermes", "resume"]).unwrap();
    match cli.command {
        Some(CliCommand::Resume { session_id }) => {
            assert!(session_id.is_none());
        }
        _ => panic!("Expected Resume command"),
    }
}

#[test]
fn cli_parse_resume_specific_session() {
    let cli = Cli::try_parse_from(vec!["hermes", "resume", "abc123"]).unwrap();
    match cli.command {
        Some(CliCommand::Resume { session_id }) => {
            assert_eq!(session_id.as_deref(), Some("abc123"));
        }
        _ => panic!("Expected Resume command"),
    }
}

#[test]
fn cli_parse_elite_check() {
    let cli = Cli::try_parse_from(vec!["hermes", "elite-check", "--json", "--strict"]).unwrap();
    match cli.command {
        Some(CliCommand::EliteCheck { json, strict }) => {
            assert!(json);
            assert!(strict);
        }
        _ => panic!("Expected EliteCheck command"),
    }
}

#[test]
fn cli_parse_systems_mcp_conformance() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "systems",
        "mcp",
        "conformance",
        "--json",
        "--output",
        "/tmp/mcp.json",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::Systems {
            action,
            topic,
            json,
            output,
            host,
            port,
            once,
        }) => {
            assert_eq!(action.as_deref(), Some("mcp"));
            assert_eq!(topic.as_deref(), Some("conformance"));
            assert!(json);
            assert_eq!(output.as_deref(), Some("/tmp/mcp.json"));
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 9127);
            assert!(!once);
        }
        _ => panic!("Expected Systems command"),
    }
}

#[test]
fn cli_parse_mcp_add_command_does_not_clobber_top_level_command() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "mcp",
        "add",
        "filesystem",
        "--command",
        "npx",
        "--parallel-tools",
    ])
    .unwrap();

    match cli.command {
        Some(CliCommand::Mcp {
            action,
            name,
            server,
            url,
            command,
            parallel_tools,
        }) => {
            assert_eq!(action.as_deref(), Some("add"));
            assert_eq!(name.as_deref(), Some("filesystem"));
            assert!(server.is_none());
            assert!(url.is_none());
            assert_eq!(command.as_deref(), Some("npx"));
            assert!(parallel_tools);
        }
        _ => panic!("Expected MCP add command"),
    }
}

#[test]
fn cli_parse_logs_default() {
    let cli = Cli::try_parse_from(vec!["hermes", "logs"]).unwrap();
    match cli.command {
        Some(CliCommand::Logs { lines, follow }) => {
            assert_eq!(lines, 20);
            assert!(!follow);
        }
        _ => panic!("Expected Logs command"),
    }
}

#[test]
fn cli_parse_logs_with_count() {
    let cli = Cli::try_parse_from(vec!["hermes", "logs", "50"]).unwrap();
    match cli.command {
        Some(CliCommand::Logs { lines, .. }) => {
            assert_eq!(lines, 50);
        }
        _ => panic!("Expected Logs command"),
    }
}

#[test]
fn cli_parse_profile() {
    let cli = Cli::try_parse_from(vec!["hermes", "profile", "list"]).unwrap();
    match cli.command {
        Some(CliCommand::Profile { action, .. }) => {
            assert_eq!(action.as_deref(), Some("list"));
        }
        _ => panic!("Expected Profile command"),
    }
}

#[test]
fn cli_parse_profile_create() {
    let cli = Cli::try_parse_from(vec!["hermes", "profile", "create", "work"]).unwrap();
    match cli.command {
        Some(CliCommand::Profile { action, name, .. }) => {
            assert_eq!(action.as_deref(), Some("create"));
            assert_eq!(name.as_deref(), Some("work"));
        }
        _ => panic!("Expected Profile command"),
    }
}

#[test]
fn cli_parse_config_set() {
    let cli = Cli::try_parse_from(vec!["hermes", "config", "set", "model", "gpt-4o"]).unwrap();
    match cli.command {
        Some(CliCommand::Config { action, key, value }) => {
            assert_eq!(action.as_deref(), Some("set"));
            assert_eq!(key.as_deref(), Some("model"));
            assert_eq!(value.as_deref(), Some("gpt-4o"));
        }
        _ => panic!("Expected Config command"),
    }
}

#[test]
fn cli_parse_billing_trailing_args() {
    let cli = Cli::try_parse_from(vec![
        "hermes",
        "billing",
        "charge",
        "50",
        "--confirm",
        "--idempotency-key=key-1",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::Billing { args }) => {
            assert_eq!(
                args,
                vec!["charge", "50", "--confirm", "--idempotency-key=key-1"]
            );
        }
        _ => panic!("Expected Billing command"),
    }
}

#[test]
fn cli_parse_secrets_set() {
    let cli = Cli::try_parse_from(vec![
        "hermes-agent-ultra",
        "secrets",
        "set",
        "openai",
        "--value",
        "sk-test",
    ])
    .unwrap();
    match cli.command {
        Some(CliCommand::Secrets {
            action,
            provider,
            value,
            show,
        }) => {
            assert_eq!(action.as_deref(), Some("set"));
            assert_eq!(provider.as_deref(), Some("openai"));
            assert_eq!(value.as_deref(), Some("sk-test"));
            assert!(!show);
        }
        _ => panic!("Expected Secrets command"),
    }
}

#[test]
fn cli_parse_skills_blank_slate_flags() {
    let cli =
        Cli::try_parse_from(vec!["hermes", "skills", "opt-out", "--remove", "--yes"]).unwrap();
    match cli.command {
        Some(CliCommand::Skills {
            action,
            remove,
            yes,
            sync,
            ..
        }) => {
            assert_eq!(action.as_deref(), Some("opt-out"));
            assert!(remove);
            assert!(yes);
            assert!(!sync);
        }
        _ => panic!("Expected Skills opt-out command"),
    }

    let cli = Cli::try_parse_from(vec!["hermes", "skills", "opt-in", "--sync"]).unwrap();
    match cli.command {
        Some(CliCommand::Skills {
            action,
            remove,
            yes,
            sync,
            ..
        }) => {
            assert_eq!(action.as_deref(), Some("opt-in"));
            assert!(!remove);
            assert!(!yes);
            assert!(sync);
        }
        _ => panic!("Expected Skills opt-in command"),
    }
}
