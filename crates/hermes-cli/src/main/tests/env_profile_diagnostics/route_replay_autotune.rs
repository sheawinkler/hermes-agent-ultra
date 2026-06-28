#[test]
fn route_health_tier_marks_failure_streak_critical() {
    let stats = RouteLearningStatsRecord {
        samples: 8,
        success_rate: 0.61,
        avg_latency_ms: 2200.0,
        consecutive_failures: 6,
        updated_at_unix_ms: 1_700_000_000_000,
    };
    let (tier, reasons, score) = route_health_tier(&stats, route_learning_score(&stats));
    assert_eq!(tier, "critical");
    assert!(reasons.iter().any(|r| r == "failure_streak_critical"));
    assert!(score >= 0.0);
}

#[test]
fn replay_integrity_detects_chain_break() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let replay = tmp.path().join("session.jsonl");
    std::fs::write(
        &replay,
        r#"{"seq":1,"event":"a","prev_hash":"seed","event_hash":"h1","payload":{"ok":true}}
{"seq":2,"event":"b","prev_hash":"BROKEN","event_hash":"h2","payload":{"ok":true}}
"#,
    )
    .expect("write replay");

    let summary = replay_integrity_for_file(&replay);
    assert_eq!(summary.events, 2);
    assert!(!summary.hash_chain_ok);
}

#[test]
fn replay_manifest_aggregates_counts() {
    let items = vec![
        ReplayIntegritySummary {
            file: "a.jsonl".to_string(),
            checksum_sha256: Some("abc".to_string()),
            events: 3,
            invalid_lines: 0,
            hash_chain_ok: true,
            last_event_hash: Some("h1".to_string()),
        },
        ReplayIntegritySummary {
            file: "b.jsonl".to_string(),
            checksum_sha256: Some("def".to_string()),
            events: 2,
            invalid_lines: 1,
            hash_chain_ok: false,
            last_event_hash: Some("h2".to_string()),
        },
    ];
    let manifest = replay_manifest_json(&items);
    assert_eq!(manifest["totals"]["files"], 2);
    assert_eq!(manifest["totals"]["events"], 5);
    assert_eq!(manifest["totals"]["invalid_lines"], 1);
    assert_eq!(manifest["totals"]["hash_chain_ok"], false);
}

#[test]
fn parse_simple_env_file_supports_export_lines() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env_path = tmp.path().join("route-autotune.env");
    std::fs::write(
        &env_path,
        "# comment\nexport HERMES_SMART_ROUTING_LEARNING_ALPHA=0.240\nHERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS=0.110\n",
    )
    .expect("write env");
    let parsed = parse_simple_env_file(&env_path);
    assert_eq!(
        parsed
            .get("HERMES_SMART_ROUTING_LEARNING_ALPHA")
            .map(String::as_str),
        Some("0.240")
    );
    assert_eq!(
        parsed
            .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
            .map(String::as_str),
        Some("0.110")
    );
}

#[test]
fn apply_route_autotune_env_overrides_sets_missing_keys_only() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "status",
    ]);
    let env_path = route_autotune_env_path_for_cli(&cli);
    if let Some(parent) = env_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(
        &env_path,
        "HERMES_SMART_ROUTING_LEARNING_ALPHA=0.300\nHERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN=0.050\n",
    )
    .expect("write env");

    std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
    std::env::set_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN", "0.999");
    let applied = apply_route_autotune_env_overrides(&cli);
    assert!(applied
        .iter()
        .any(|k| k == "HERMES_SMART_ROUTING_LEARNING_ALPHA"));
    assert_eq!(
        std::env::var("HERMES_SMART_ROUTING_LEARNING_ALPHA").ok(),
        Some("0.300".to_string())
    );
    assert_eq!(
        std::env::var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN").ok(),
        Some("0.999".to_string()),
        "explicit env var should not be overridden"
    );
    std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
    std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN");
}

#[test]
fn build_route_autotune_plan_raises_bias_for_critical_health() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "status",
    ]);
    let entry = RouteHealthEntry {
        key: "openai:gpt-4o".to_string(),
        health_score: 0.2,
        tier: "critical".to_string(),
        reasons: vec!["failure_streak_critical".to_string()],
        stats: RouteLearningStatsRecord {
            samples: 9,
            success_rate: 0.4,
            avg_latency_ms: 5200.0,
            consecutive_failures: 7,
            updated_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        },
    };
    let summary = serde_json::json!({
        "entries": 1,
        "overall": "critical",
        "average_score": 0.2,
        "healthy": 0,
        "watch": 0,
        "degraded": 0,
        "critical": 1
    });
    let plan = build_route_autotune_plan(
        &cli,
        Path::new("/tmp/route-learning.json"),
        Path::new("/tmp/route-health.json"),
        &[entry],
        &summary,
    );
    let cheap_bias = plan
        .overrides
        .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let switch_margin = plan
        .overrides
        .get("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    assert!(cheap_bias >= 0.14);
    assert!(switch_margin >= 0.05);
    assert_eq!(plan.confidence, "low");
}
