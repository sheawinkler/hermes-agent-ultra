#[test]
fn doctor_self_heal_creates_missing_state_dirs() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let state_root = hermes_state_root(&cli);
    assert!(!state_root.join("profiles").exists());

    let actions = run_doctor_self_heal(&cli);
    assert!(state_root.join("profiles").exists());
    assert!(state_root.join("sessions").exists());
    assert!(state_root.join("logs").exists());
    assert!(actions
        .iter()
        .any(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("created")));
}

#[test]
fn doctor_self_heal_removes_stale_gateway_pid_file() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let pid_path = gateway_pid_path_for_cli(&cli);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir pid dir");
    }
    std::fs::write(&pid_path, "999999").expect("write stale pid");
    assert!(pid_path.exists());

    let actions = run_doctor_self_heal(&cli);
    assert!(!pid_path.exists(), "stale pid file should be removed");
    assert!(actions.iter().any(|entry| {
        entry
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("removed stale gateway pid file")
    }));
}

#[test]
fn doctor_elite_diagnostics_payload_has_required_sections() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let payload = build_elite_doctor_diagnostics(&cli);
    assert!(payload.get("provenance").is_some());
    assert!(payload.get("route_learning").is_some());
    assert!(payload.get("route_health").is_some());
    assert!(payload.get("tool_policy").is_some());
    assert!(payload.get("elite_gate").is_some());
}

#[test]
fn resolve_resume_session_file_prefers_latest_modified_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let old = sessions_dir.join("old-session.json");
    let new = sessions_dir.join("new-session.json");
    std::fs::write(&old, r#"{"messages":[{"role":"user","content":"old"}]}"#)
        .expect("write old session");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&new, r#"{"messages":[{"role":"user","content":"new"}]}"#)
        .expect("write new session");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(resolved, "new-session");
    assert_eq!(path, new);
}

#[test]
fn resolve_resume_session_file_latest_prefers_canonical_session_stem() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let canonical = sessions_dir.join("c0ffee00-0000-4000-8000-000000000001.json");
    std::fs::write(
        &canonical,
        r#"{
  "session_info": {"session_id":"c0ffee00-0000-4000-8000-000000000001","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write canonical");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let named = sessions_dir.join("newest.json");
    std::fs::write(
        &named,
        r#"{
  "session_info": {"session_id":"snap-prune","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"snapshot payload"}]
}"#,
    )
    .expect("write named artifact");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(resolved, "c0ffee00-0000-4000-8000-000000000001");
    assert_eq!(path, canonical);
}

#[test]
fn resolve_resume_session_file_searches_session_id_when_exact_file_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let snapshot = sessions_dir.join("saved-snapshot-name.json");
    std::fs::write(
        &snapshot,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"}]
}"#,
    )
    .expect("write snapshot");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("20260603")).expect("resolve prefix");
    assert_eq!(resolved, "saved-snapshot-name");
    assert_eq!(path, snapshot);

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("ABCD12")).expect("resolve substring");
    assert_eq!(resolved, "saved-snapshot-name");
    assert_eq!(path, snapshot);
}

#[test]
fn resolve_resume_session_file_search_ranks_exact_before_prefix() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let exact = sessions_dir.join("snap-exact.json");
    std::fs::write(
        &exact,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"exact"}]
}"#,
    )
    .expect("write exact");
    let prefix = sessions_dir.join("20260603_090200_abcd12_child.json");
    std::fs::write(
        &prefix,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12_child","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"prefix"}]
}"#,
    )
    .expect("write prefix");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("20260603_090200_abcd12"))
            .expect("resolve exact session_info id");
    assert_eq!(resolved, "snap-exact");
    assert_eq!(path, exact);
}

#[test]
fn should_resume_fallback_to_fresh_only_for_latest_missing_state() {
    let latest_missing = AgentError::Config("No saved sessions found in /tmp".to_string());
    assert!(should_resume_fallback_to_fresh(None, &latest_missing));
    assert!(should_resume_fallback_to_fresh(
        Some("latest"),
        &latest_missing
    ));
    assert!(!should_resume_fallback_to_fresh(
        Some("abc123"),
        &latest_missing
    ));

    let other_error = AgentError::Config("Session 'abc123' not found".to_string());
    assert!(!should_resume_fallback_to_fresh(None, &other_error));
}

#[test]
fn load_resume_payload_restores_metadata_and_messages() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let session_path = sessions_dir.join("abc123.json");
    std::fs::write(
        &session_path,
        r#"{
  "session_info": {
"session_id": "session-xyz",
"model": "nous:openai/gpt-5.5-pro",
"personality": "technical"
  },
  "messages": [
{"role":"System","content":"[SESSION_OBJECTIVE] Keep context fresh"},
{"role":"User","content":"hello"},
{"role":"Assistant","content":"world"}
  ]
}"#,
    )
    .expect("write session");

    let payload = load_resume_payload(&cli, Some("abc123")).expect("load payload");
    assert_eq!(payload.resolved_id, "abc123");
    assert_eq!(payload.session_id, "session-xyz");
    assert_eq!(payload.model.as_deref(), Some("nous:openai/gpt-5.5-pro"));
    assert_eq!(payload.personality.as_deref(), Some("technical"));
    assert_eq!(payload.messages.len(), 3);
    assert!(matches!(
        payload.messages[0].role,
        hermes_core::MessageRole::System
    ));
    assert!(matches!(
        payload.messages[1].role,
        hermes_core::MessageRole::User
    ));
    assert!(matches!(
        payload.messages[2].role,
        hermes_core::MessageRole::Assistant
    ));
}

#[test]
fn load_resume_payload_follows_compression_tip_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let state_root = hermes_state_root(&cli);
    let sessions_dir = state_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("root.json"),
        r#"{
  "session_info": {"session_id":"root","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"pre-compression turn"}]
}"#,
    )
    .expect("write root session");
    std::fs::write(
        sessions_dir.join("cont.json"),
        r#"{
  "session_info": {"session_id":"cont","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"Assistant","content":"post-compression reply"}]
}"#,
    )
    .expect("write continuation session");

    let persistence = SessionPersistence::new(&state_root);
    persistence
        .persist_session(
            "root",
            &[hermes_core::Message::user("pre-compression turn")],
            Some("nous:openai/gpt-5.5"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    persistence
        .persist_session(
            "cont",
            &[hermes_core::Message::assistant("post-compression reply")],
            Some("nous:openai/gpt-5.5"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    let base = chrono::Utc::now() - chrono::Duration::hours(1);
    let root_created = base.to_rfc3339();
    let root_ended = (base + chrono::Duration::seconds(10)).to_rfc3339();
    let cont_created = (base + chrono::Duration::seconds(20)).to_rfc3339();
    assert!(persistence
        .update_session_lineage(
            "root",
            None,
            Some("compression"),
            Some(&root_created),
            Some(&root_ended),
        )
        .expect("mark root compressed"));
    assert!(persistence
        .update_session_lineage("cont", Some("root"), None, Some(&cont_created), None,)
        .expect("link continuation"));

    let payload = load_resume_payload(&cli, Some("root")).expect("load payload");

    assert_eq!(payload.resolved_id, "cont");
    assert_eq!(payload.session_id, "cont");
    assert_eq!(payload.source_path, sessions_dir.join("cont.json"));
    assert_eq!(payload.messages.len(), 1);
    assert_eq!(
        payload.messages[0].content.as_deref(),
        Some("post-compression reply")
    );
}

#[test]
fn load_resume_payload_falls_back_to_legacy_sessions_dir() {
    let _guard = env_lock();
    let prev_home = std::env::var("HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path().join("fake-home");
    let legacy_sessions = fake_home.join(".hermes").join("sessions");
    std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");
    let legacy_path = legacy_sessions.join("legacy-abc.json");
    std::fs::write(
        &legacy_path,
        r#"{
  "session_info": {
"session_id": "legacy-session",
"model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": [
{"role":"User","content":"from-legacy"}
  ]
}"#,
    )
    .expect("write legacy session");

    std::env::set_var("HOME", &fake_home);
    let state_root = tmp.path().join("ultra-state");
    let cli = cli_for_temp_state_root(&state_root);
    let payload = load_resume_payload(&cli, Some("legacy-abc")).expect("load payload");
    assert_eq!(payload.resolved_id, "legacy-abc");
    assert_eq!(payload.session_id, "legacy-session");
    assert_eq!(payload.messages.len(), 1);
    assert!(payload.source_path.starts_with(&legacy_sessions));

    match prev_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn load_resume_payload_accepts_empty_messages_for_startup_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let session_path = sessions_dir.join("empty-messages.json");
    std::fs::write(
        &session_path,
        r#"{
  "session_info": {
"session_id": "empty-messages",
"model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": []
}"#,
    )
    .expect("write empty session");

    let payload = load_resume_payload(&cli, Some("empty-messages")).expect("load payload");
    assert_eq!(payload.resolved_id, "empty-messages");
    assert_eq!(payload.session_id, "empty-messages");
    assert_eq!(
        payload.model.as_deref(),
        Some("nous:nousresearch/hermes-4-70b")
    );
    assert_eq!(payload.messages.len(), 0);
}

#[test]
fn load_resume_payload_latest_prefers_nonempty_snapshot_over_newer_empty_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let non_empty = sessions_dir.join("history-real.json");
    std::fs::write(
        &non_empty,
        r#"{
  "session_info": {"session_id":"history-real","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"},{"role":"Assistant","content":"world"}]
}"#,
    )
    .expect("write non-empty session");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let empty_snapshot = sessions_dir.join("startup-empty.json");
    std::fs::write(
        &empty_snapshot,
        r#"{
  "session_info": {"session_id":"startup-empty","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write empty session");

    let payload = load_resume_payload(&cli, None).expect("load payload");
    assert_eq!(payload.resolved_id, "history-real");
    assert_eq!(payload.messages.len(), 2);
    assert_eq!(payload.source_path, non_empty);
}

#[test]
fn load_resume_payload_latest_falls_back_to_legacy_nonempty_when_primary_empty_only() {
    let _guard = env_lock();
    let prev_home = std::env::var("HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path().join("fake-home");
    let legacy_sessions = fake_home.join(".hermes").join("sessions");
    std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");

    let legacy_non_empty = legacy_sessions.join("legacy-rich.json");
    std::fs::write(
        &legacy_non_empty,
        r#"{
  "session_info": {"session_id":"legacy-rich","model":"nous:nousresearch/hermes-4-70b"},
  "messages":[{"role":"User","content":"from legacy"}]
}"#,
    )
    .expect("write legacy non-empty session");

    std::env::set_var("HOME", &fake_home);
    let state_root = tmp.path().join("ultra-state");
    let cli = cli_for_temp_state_root(&state_root);
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("empty-only.json"),
        r#"{
  "session_info": {"session_id":"empty-only","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write primary empty session");

    let payload = load_resume_payload(&cli, None).expect("load payload");
    assert_eq!(payload.resolved_id, "legacy-rich");
    assert_eq!(payload.messages.len(), 1);
    assert!(payload.source_path.starts_with(&legacy_sessions));

    match prev_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[tokio::test]
async fn run_dump_writes_real_saved_session_export_with_system_prompt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("abc123.json"),
        r#"{
  "session_info": {
"session_id": "session-xyz",
"model": "nous:openai/gpt-5.5",
"personality": "technical",
"created_at": "2026-06-05T09:00:00Z"
  },
  "system_prompt": "persisted system prompt",
  "messages": [
{"role":"User","content":"hello"},
{"role":"Assistant","content":"world"}
  ]
}"#,
    )
    .expect("write session");

    run_dump(cli, Some("abc123".to_string()), None)
        .await
        .expect("dump session");

    let saved_dir = tmp.path().join("sessions").join("saved");
    let entries = std::fs::read_dir(&saved_dir)
        .expect("saved dir")
        .collect::<Result<Vec<_>, _>>()
        .expect("saved entries");
    assert_eq!(entries.len(), 1);
    let path = entries[0].path();
    assert!(path
        .file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|name| name.starts_with("hermes_conversation_")));

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read dump"))
            .expect("parse dump");
    assert_eq!(doc["session_id"], "session-xyz");
    assert_eq!(doc["resolved_id"], "abc123");
    assert_eq!(doc["model"], "nous:openai/gpt-5.5");
    assert_eq!(doc["personality"], "technical");
    assert_eq!(doc["system_prompt"], "persisted system prompt");
    assert_eq!(doc["session_start"], "2026-06-05T09:00:00Z");
    assert_eq!(doc["messages"].as_array().map(Vec::len), Some(2));
    assert!(doc["source_path"]
        .as_str()
        .is_some_and(|p| p.ends_with("abc123.json")));
}

