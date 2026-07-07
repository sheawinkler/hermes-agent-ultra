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

    assert!(!DiscordSlashSyncPolicy::Off.should_register(true));
    assert!(!DiscordSlashSyncPolicy::Bulk.should_register(false));
    assert!(DiscordSlashSyncPolicy::parse(Some("bulk")).should_register(true));
    assert_eq!(
        DiscordSlashSyncPolicy::parse(Some("unknown")),
        DiscordSlashSyncPolicy::Diff
    );
}

#[test]
fn discord_command_sync_plans_diffs_recreates_and_deletes() {
    let desired = vec![
        serde_json::json!({
            "name": "status",
            "description": "Show Hermes status",
            "type": 1,
            "options": [],
            "nsfw": false,
            "dm_permission": true,
            "default_member_permissions": null,
        }),
        serde_json::json!({
            "name": "help",
            "description": "Show available commands",
            "type": 1,
            "options": [],
            "nsfw": false,
            "dm_permission": true,
        }),
        serde_json::json!({
            "name": "metricas",
            "description": "Metrics dashboard",
            "type": 1,
            "options": [],
        }),
        serde_json::json!({
            "name": "admin",
            "description": "Admin-only command",
            "type": 1,
            "options": [],
            "nsfw": true,
            "dm_permission": false,
            "default_member_permissions": "8",
        }),
        serde_json::json!({
            "name": "contexts",
            "description": "Context drift check",
            "type": 1,
            "options": [],
            "contexts": [0, 1, 2],
            "integration_types": [0, 1],
        }),
    ];
    let existing = vec![
        serde_json::json!({
            "id": 11,
            "application_id": 999,
            "name": "status",
            "description": "Show Hermes status",
            "type": 1,
            "options": [],
            "nsfw": false,
            "dm_permission": true,
            "default_member_permissions": null,
            "name_localizations": {},
            "description_localizations": {},
        }),
        serde_json::json!({
            "id": 12,
            "application_id": 999,
            "name": "help",
            "description": "Old help text",
            "type": 1,
            "options": [],
            "nsfw": false,
            "dm_permission": true,
        }),
        serde_json::json!({
            "id": 13,
            "name": "old-command",
            "description": "To be deleted",
            "type": 1,
            "options": [],
        }),
        serde_json::json!({
            "id": 14,
            "name": "admin",
            "description": "Admin-only command",
            "type": 1,
            "options": [],
            "nsfw": true,
            "dm_permission": false,
        }),
        serde_json::json!({
            "id": 15,
            "name": "contexts",
            "description": "Context drift check",
            "type": 1,
            "options": [],
            "contexts": [0],
            "integration_types": [0],
        }),
    ];

    let summary = plan_discord_command_sync(&desired, &existing);

    assert_eq!(summary.total, 5);
    assert_eq!(summary.unchanged, 1);
    assert_eq!(summary.updated, 1);
    assert_eq!(summary.recreated, 2);
    assert_eq!(summary.created, 1);
    assert_eq!(summary.deleted, 1);
    assert_eq!(
        summary.mutations.first(),
        Some(&DiscordCommandSyncMutation::Delete {
            name: "old-command".into()
        })
    );
    let delete_index = summary
        .mutations
        .iter()
        .position(|mutation| {
            mutation
                == &DiscordCommandSyncMutation::Delete {
                    name: "old-command".into(),
                }
        })
        .expect("obsolete command delete mutation");
    let create_index = summary
        .mutations
        .iter()
        .position(|mutation| {
            mutation
                == &DiscordCommandSyncMutation::Create {
                    name: "metricas".into(),
                }
        })
        .expect("new command create mutation");
    assert!(delete_index < create_index);
    assert!(summary
        .mutations
        .contains(&DiscordCommandSyncMutation::Update {
            name: "help".into()
        }));
    assert!(summary
        .mutations
        .contains(&DiscordCommandSyncMutation::Recreate {
            name: "admin".into()
        }));
    assert!(summary
        .mutations
        .contains(&DiscordCommandSyncMutation::Recreate {
            name: "contexts".into()
        }));
    assert!(summary
        .mutations
        .contains(&DiscordCommandSyncMutation::Create {
            name: "metricas".into()
        }));
    assert!(summary
        .mutations
        .contains(&DiscordCommandSyncMutation::Delete {
            name: "old-command".into()
        }));
}

#[test]
fn discord_command_sync_state_skips_same_fingerprint_and_honors_retry_after() {
    let commands = vec![serde_json::json!({
        "name": "status",
        "description": "Show Hermes status",
        "type": 1,
        "options": [],
    })];
    let fingerprint = discord_command_fingerprint(&commands);
    let mut state = DiscordCommandSyncStateEntry::default();

    assert!(state.should_attempt(&fingerprint, 10));
    state.record_attempt(10);
    state.record_success(fingerprint.clone(), 11);
    assert!(!state.should_attempt(&fingerprint, 12));

    let changed = discord_command_fingerprint(&[serde_json::json!({
        "name": "status",
        "description": "Show current Hermes status",
        "type": 1,
        "options": [],
    })]);
    assert!(state.should_attempt(&changed, 13));
    state.record_rate_limit(123, 20);
    assert!(!state.should_attempt(&changed, 100));
    assert!(state.should_attempt(&changed, 144));
    assert_eq!(state.retry_after, Some(123));
    assert_eq!(state.retry_after_until, Some(143));
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
fn discord_auto_registration_skips_conflicts_and_dispatches_args() {
    let gateway = vec![
        DiscordSlashRegistrationSpec::new(
            "debug",
            "Generate a debug report",
            None::<String>,
            "/debug",
        ),
        DiscordSlashRegistrationSpec::new(
            "branch",
            "Show or switch branch",
            Some("[name]"),
            "/branch",
        ),
    ];
    let plugins = vec![
        DiscordSlashRegistrationSpec::new(
            "status",
            "Plugin status",
            None::<String>,
            "/status-plugin",
        ),
        DiscordSlashRegistrationSpec::new(
            "metricas",
            "Metrics dashboard",
            Some("dias:7 formato:json"),
            "/metricas",
        ),
    ];

    let registered = discord_auto_registered_commands(["status", "thread"], gateway, plugins);
    let names = registered
        .iter()
        .map(|spec| spec.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["debug", "branch", "metricas"]);
    assert_eq!(registered[0].dispatch_text(None), "/debug");
    assert_eq!(
        registered[1].dispatch_text(Some("my-branch")),
        "/branch my-branch"
    );
    assert_eq!(
        registered[2].dispatch_text(Some("dias:7 formato:json")),
        "/metricas dias:7 formato:json"
    );

    let slash = registered[1].to_slash_command();
    assert_eq!(slash.name, "branch");
    assert_eq!(slash.options.as_ref().map(Vec::len), Some(1));
}

#[test]
fn discord_auto_registration_caps_at_discord_command_limit() {
    let gateway = (0..120)
        .map(|idx| {
            DiscordSlashRegistrationSpec::new(
                format!("gateway-{idx}"),
                format!("Gateway command {idx}"),
                None::<String>,
                format!("/gateway-{idx}"),
            )
        })
        .collect::<Vec<_>>();
    let plugins = vec![DiscordSlashRegistrationSpec::new(
        "plugin-extra",
        "Plugin command",
        None::<String>,
        "/plugin-extra",
    )];

    let registered = discord_auto_registered_commands(["status", "thread"], gateway, plugins);
    let names = registered
        .iter()
        .map(|spec| spec.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(registered.len(), DISCORD_APPLICATION_COMMAND_LIMIT - 2);
    assert_eq!(names.first().copied(), Some("gateway-0"));
    assert_eq!(names.last().copied(), Some("gateway-97"));
    assert!(!names.contains(&"plugin-extra"));
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
