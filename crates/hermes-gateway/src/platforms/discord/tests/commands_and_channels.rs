#[test]
fn discord_connect_policy_matches_members_intent_and_sync_opt_out() {
    assert!(!discord_members_intent_required(["769524422783664158"]));
    assert!(discord_members_intent_required(["abhey-gupta"]));
    assert!(discord_members_intent_required([
        "769524422783664158",
        "abhey-gupta"
    ]));

    assert_eq!(
        discord_client_reentry_action(false),
        DiscordClientReentryAction::ReuseFreshSlot
    );
    assert_eq!(
        discord_client_reentry_action(true),
        DiscordClientReentryAction::ClosePreviousClient
    );

}


#[test]
fn discord_channel_prompt_and_model_picker_contracts_match_python_order() {
    let prompts = BTreeMap::from([
        ("200".to_string(), "Parent prompt".to_string()),
        ("999".to_string(), "Thread prompt".to_string()),
        ("blank".to_string(), "   ".to_string()),
    ]);

    assert_eq!(
        discord_resolve_channel_prompt(&prompts, "999", Some("200")),
        Some("Thread prompt")
    );
    assert_eq!(
        discord_resolve_channel_prompt(&prompts, "123", Some("200")),
        Some("Parent prompt")
    );
    assert_eq!(
        discord_resolve_channel_prompt(&prompts, "blank", None),
        None
    );
    assert_eq!(
        discord_compose_ephemeral_system_prompt(
            Some("Context prompt"),
            Some("Channel prompt"),
            Some("Global prompt"),
        )
        .as_deref(),
        Some("Context prompt\n\nChannel prompt\n\nGlobal prompt")
    );

    let (initial, final_edit) = discord_model_picker_switch_edits("gpt-5.4", "Model switched");
    assert_eq!(initial.title, "Switching Model");
    assert_eq!(initial.description, "Switching to `gpt-5.4`...");
    assert!(initial.clears_view);
    assert_eq!(final_edit.title, "Model Switched");
    assert_eq!(final_edit.description, "Model switched");
    assert!(final_edit.clears_view);
}

#[test]
fn discord_auto_thread_names_and_feedback_match_slash_contract() {
    assert_eq!(
        discord_auto_thread_name("<@&1490963422786093149> <@555> please help <#123>"),
        "please help"
    );
    assert_eq!(
        discord_auto_thread_name("<@&1490963422786093149>"),
        "Hermes"
    );
    let long_name = discord_auto_thread_name(&"a".repeat(200));
    assert_eq!(long_name.len(), 80);
    assert!(long_name.ends_with("..."));
    let emoji_name = discord_auto_thread_name(&"😀".repeat(80));
    assert!(discord_utf16_len(&emoji_name) <= 80);
    assert!(emoji_name.ends_with("..."));
    assert!(discord_thread_create_success_message("555").contains("<#555>"));
    assert!(discord_thread_create_failure_message("nope").contains("Failed to create thread"));
}

#[test]
fn discord_attachment_document_opus_and_voice_contracts_match_upstream_cases() {
    let image = discord_attachment_handling("file.png", Some("image/png"), 64);
    assert_eq!(image.kind, DiscordAttachmentKind::Image);
    assert!(image.prefer_bot_session_read);
    assert!(image.fallback_uses_ssrf_gate);
    assert!(!image.inject_text_content);

    let audio = discord_attachment_handling("voice.ogg", Some("audio/ogg"), 64);
    assert_eq!(audio.kind, DiscordAttachmentKind::Audio);
    assert!(audio.prefer_bot_session_read);

    let txt = discord_attachment_handling("notes.txt", Some("text/plain"), 1024);
    assert_eq!(txt.kind, DiscordAttachmentKind::Document);
    assert!(txt.prefer_bot_session_read);
    assert!(txt.fallback_uses_ssrf_gate);
    assert!(txt.inject_text_content);
    assert_eq!(
        discord_inject_document_text("summarize this", "notes.txt", "Hello"),
        "[Content of notes.txt]:\nHello\n\nsummarize this"
    );

    let pdf = discord_attachment_handling("report.pdf", Some("application/pdf"), 1024);
    assert_eq!(pdf.kind, DiscordAttachmentKind::Document);
    assert!(!pdf.inject_text_content);

    assert_eq!(
        discord_opus_library_candidates("linux", Some("libopus.so")),
        vec!["libopus.so".to_string()]
    );
    let mac_fallbacks = discord_opus_library_candidates("darwin", None);
    assert!(mac_fallbacks[0].contains("/opt/homebrew"));
    assert!(discord_should_log_opus_decode_error(Some("decode failed")));

    let mut joins = DiscordVoiceJoinTracker::default();
    assert_eq!(joins.begin_join("42"), DiscordVoiceJoinAction::Connect);
    assert_eq!(joins.begin_join("42"), DiscordVoiceJoinAction::MoveExisting);
    joins.complete_join("42", true);
    assert_eq!(
        joins.begin_join("42"),
        DiscordVoiceJoinAction::AlreadyConnected
    );
}


#[test]
fn discord_channel_skill_bindings_resolve_exact_parent_and_deduped_skills() {
    let bindings = DiscordChannelSkillBinding::list_from_json(Some(&serde_json::json!([
        {"id": "100", "skills": ["a", "b", "a", "c", "b"]},
        {"id": "200", "skill": "forum-skill"},
        {"id": 300, "skills": "solo"},
    ])));

    assert_eq!(
        resolve_channel_skills_from_bindings(&bindings, "100", None),
        Some(vec!["a".into(), "b".into(), "c".into()])
    );
    assert_eq!(
        resolve_channel_skills_from_bindings(&bindings, "999", Some("200")),
        Some(vec!["forum-skill".into()])
    );
    assert_eq!(
        resolve_channel_skills_from_bindings(&bindings, "300", None),
        Some(vec!["solo".into()])
    );
    assert_eq!(
        resolve_channel_skills_from_bindings(&bindings, "999", None),
        None
    );
}

#[test]
fn discord_adapter_resolves_configured_channel_skills() {
    let mut cfg = test_config();
    cfg.channel_skill_bindings = DiscordChannelSkillBinding::list_from_json(Some(
        &serde_json::json!([{"id": "100", "skills": ["skill-a", "skill-b"]}]),
    ));
    let adapter = DiscordAdapter::new(cfg).unwrap();
    assert_eq!(
        adapter.resolve_channel_skills("100", None),
        Some(vec!["skill-a".into(), "skill-b".into()])
    );
    assert_eq!(adapter.resolve_channel_skills("101", None), None);
}

#[test]
fn discord_adapter_explicit_state_root_isolates_both_trackers_from_global_home() {
    let state = tempfile::tempdir().expect("isolated selected state root");
    let mut cfg = test_config();
    cfg.application_id = Some(format!("state-root-{}", uuid::Uuid::new_v4()));
    let scope = DiscordAdapter::state_scope_for_config(&cfg);
    let state_root = state.path().join("discord").join(&scope);
    let global_thread_path = hermes_config::hermes_home().join(format!("{scope}_threads.json"));
    let global_message_path = hermes_config::hermes_home()
        .join("gateway")
        .join(format!("{scope}_discord_nonconversational_messages.json"));

    let adapter = DiscordAdapter::new_with_state_root(cfg, &state_root)
        .expect("adapter with explicit tracker state root");
    let thread_path = adapter
        .thread_participation
        .lock()
        .expect("thread tracker lock")
        .path()
        .to_path_buf();
    let message_path = adapter
        .non_conversational_messages
        .lock()
        .expect("message tracker lock")
        .path
        .to_path_buf();

    adapter
        .thread_participation
        .lock()
        .expect("thread tracker lock")
        .mark("thread-private")
        .expect("persist thread tracker");
    adapter
        .non_conversational_messages
        .lock()
        .expect("message tracker lock")
        .mark_many(["message-private"])
        .expect("persist message tracker");

    assert_eq!(thread_path, state_root.join("threads.json"));
    assert_eq!(
        message_path,
        state_root.join("discord_nonconversational_messages.json")
    );
    assert!(thread_path.exists());
    assert!(message_path.exists());
    assert!(
        !global_thread_path.exists() && !global_message_path.exists(),
        "explicit state root must never write profile-scoped IDs under global hermes_home"
    );
}

#[test]
fn discord_thread_participation_tracker_persists_and_keeps_newest() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("discord_threads.json");
    let mut tracker = DiscordThreadParticipationTracker::from_path(&path, 5);

    assert!(tracker.is_empty());
    assert!(tracker.mark("0").unwrap());
    assert!(!tracker.mark("0").unwrap());
    for id in ["1", "2", "3", "4", "newest"] {
        assert!(tracker.mark(id).unwrap());
    }

    assert_eq!(tracker.entries(), vec!["1", "2", "3", "4", "newest"]);
    let saved: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved, vec!["1", "2", "3", "4", "newest"]);

    let reloaded = DiscordThreadParticipationTracker::from_path(&path, 5);
    assert!(reloaded.contains("newest"));
    assert!(!reloaded.contains("0"));
}

#[test]
fn discord_thread_participation_tracker_tolerates_corrupt_and_missing_state() {
    let tmp = tempfile::tempdir().unwrap();
    let corrupt_path = tmp.path().join("discord_threads.json");
    std::fs::write(&corrupt_path, "not valid json{{{").unwrap();
    let tracker = DiscordThreadParticipationTracker::from_path(&corrupt_path, 5);
    assert!(tracker.is_empty());

    let missing_parent = tmp
        .path()
        .join("missing")
        .join("deep")
        .join("discord_threads.json");
    let mut tracker = DiscordThreadParticipationTracker::from_path(&missing_parent, 5);
    assert!(tracker.mark("111").unwrap());
    assert!(missing_parent.exists());
}

#[test]
fn discord_non_conversational_tracker_persists_and_keeps_newest() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp
        .path()
        .join("gateway")
        .join("discord_nonconversational_messages.json");
    let mut tracker = DiscordNonConversationalMessageTracker::from_path(&path, 3);

    assert!(tracker.mark_many(["1", "2", "2", "3"]).unwrap());
    assert!(!tracker.mark_many(["2"]).unwrap());
    assert!(tracker.mark_many(["4"]).unwrap());
    assert_eq!(tracker.entries(), vec!["2", "3", "4"]);
    assert!(tracker.contains("4"));
    assert!(!tracker.contains("1"));

    let reloaded = DiscordNonConversationalMessageTracker::from_path(&path, 3);
    assert_eq!(reloaded.entries(), vec!["2", "3", "4"]);
}

#[test]
fn discord_trackers_replace_existing_state_files_without_removing_them() {
    let tmp = tempfile::tempdir().unwrap();

    let thread_path = tmp.path().join("threads.json");
    std::fs::write(&thread_path, r#"["old-thread"]"#).unwrap();
    let mut threads = DiscordThreadParticipationTracker::from_path(&thread_path, 4);
    assert!(threads.mark("new-thread").unwrap());
    assert_eq!(
        std::fs::read_to_string(&thread_path).unwrap(),
        r#"["old-thread","new-thread"]"#
    );

    let message_path = tmp.path().join("messages.json");
    std::fs::write(&message_path, r#"["old-message"]"#).unwrap();
    let mut messages = DiscordNonConversationalMessageTracker::from_path(&message_path, 4);
    assert!(messages.mark_many(["new-message"]).unwrap());
    assert_eq!(
        std::fs::read_to_string(&message_path).unwrap(),
        r#"["old-message","new-message"]"#
    );
}

#[cfg(unix)]
#[test]
fn discord_tracker_atomic_writes_keep_existing_and_new_state_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().expect("isolated tracker state");
    let existing_thread_path = tmp.path().join("threads.json");
    std::fs::write(&existing_thread_path, r#"["old-thread"]"#)
        .expect("write existing thread state");
    std::fs::set_permissions(&existing_thread_path, std::fs::Permissions::from_mode(0o600))
        .expect("make existing thread state private");
    let mut threads = DiscordThreadParticipationTracker::from_path(&existing_thread_path, 4);
    threads.mark("new-thread").expect("replace private thread state");

    let new_message_path = tmp.path().join("messages.json");
    let mut messages = DiscordNonConversationalMessageTracker::from_path(&new_message_path, 4);
    messages
        .mark_many(["new-message"])
        .expect("create private message state");

    assert_eq!(
        std::fs::metadata(&existing_thread_path)
            .expect("existing thread metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "atomic replacement must retain private Unix mode"
    );
    assert_eq!(
        std::fs::metadata(&new_message_path)
            .expect("new message metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "new tracker state must not inherit a group/world-readable umask mode"
    );
}

#[test]
fn discord_tracker_parent_sync_failure_is_reported_and_successful_reopen_is_durable() {
    let tmp = tempfile::tempdir().expect("isolated tracker state");
    let path = tmp.path().join("threads.json");
    let mut tracker = DiscordThreadParticipationTracker::from_path(&path, 4);

    tracker.fail_next_parent_sync_for_test();
    assert!(
        tracker.mark("unacknowledged-after-rename").is_err(),
        "a failed directory sync must not be reported as a durable save"
    );
    assert!(
        !tracker.contains("unacknowledged-after-rename"),
        "the caller's in-memory state must roll back when durability is unknown"
    );

    let mut reopened = DiscordThreadParticipationTracker::from_path(&path, 4);
    assert!(
        reopened.contains("unacknowledged-after-rename"),
        "the completed rename remains recoverable even though parent durability was not acknowledged"
    );
    reopened
        .mark("durably-synced")
        .expect("a successful parent sync persists tracker state");
    assert!(
        DiscordThreadParticipationTracker::from_path(&path, 4).contains("durably-synced"),
        "a successful atomic replacement plus parent sync must reopen with saved state"
    );
}

#[test]
fn discord_tracker_parent_sync_injection_is_instance_scoped_under_concurrent_saves() {
    let tmp = tempfile::tempdir().expect("isolated tracker state");
    let mut targeted =
        DiscordThreadParticipationTracker::from_path(tmp.path().join("targeted.json"), 4);
    let mut unaffected = DiscordNonConversationalMessageTracker::from_path(
        tmp.path().join("unaffected.json"),
        4,
    );
    targeted.fail_next_parent_sync_for_test();

    std::thread::scope(|scope| {
        let targeted_save = scope.spawn(|| targeted.mark("targeted"));
        let unaffected_save = scope.spawn(|| unaffected.mark_many(["unaffected"]));
        assert!(
            targeted_save
                .join()
                .expect("targeted save thread completes")
                .is_err(),
            "only the targeted tracker consumes its injected parent-sync failure"
        );
        assert!(
            unaffected_save
                .join()
                .expect("unaffected save thread completes")
                .expect("unaffected tracker save succeeds"),
            "a concurrent tracker must not consume another instance's injection"
        );
    });

    assert!(!targeted.contains("targeted"));
    assert!(unaffected.contains("unaffected"));
}

#[test]
fn discord_trackers_retry_persistence_after_a_transient_write_failure() {
    let tmp = tempfile::tempdir().unwrap();

    let thread_path = tmp.path().join("threads.json");
    let mut threads = DiscordThreadParticipationTracker::from_path(&thread_path, 4);
    threads.fail_next_save_for_test();
    assert!(threads.mark("thread-retry").is_err());
    assert!(
        !threads.contains("thread-retry"),
        "a failed save must roll back in-memory participation"
    );
    assert!(threads.mark("thread-retry").unwrap());
    assert!(
        DiscordThreadParticipationTracker::from_path(&thread_path, 4).contains("thread-retry"),
        "a later mark must retry persistence rather than being hidden by stale memory"
    );

    let message_path = tmp.path().join("messages.json");
    let mut messages = DiscordNonConversationalMessageTracker::from_path(&message_path, 4);
    messages.fail_next_save_for_test();
    assert!(messages.mark_many(["message-retry"]).is_err());
    assert!(
        !messages.contains("message-retry"),
        "a failed save must roll back in-memory message state"
    );
    assert!(messages.mark_many(["message-retry"]).unwrap());
    assert!(
        DiscordNonConversationalMessageTracker::from_path(&message_path, 4)
            .contains("message-retry"),
        "a later mark must persist and reload after the injected failure"
    );
}
