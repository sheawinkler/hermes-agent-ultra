use super::*;

// -- existing tests (preserved) -----------------------------------------

fn test_config() -> DiscordConfig {
    DiscordConfig {
        token: "test-token".into(),
        application_id: None,
        proxy: AdapterProxyConfig::default(),
        require_mention: false,
        intents: default_intents(),
        reply_to_mode: default_reply_to_mode(),
        channel_controls: DiscordChannelControls::default(),
        channel_skill_bindings: Vec::new(),
    }
}

#[test]
fn send_metadata_targets_thread_id_when_present() {
    let metadata = DiscordSendMetadata::with_thread_id(" 987654321 ");
    assert_eq!(metadata.target_channel_id("123"), "987654321");
    assert_eq!(metadata.reply_to_message_id(), None);
    assert!(!metadata.marks_non_conversational());

    let blank_metadata = DiscordSendMetadata::with_thread_id("   ");
    assert_eq!(blank_metadata.target_channel_id("123"), "123");
    assert_eq!(target_channel_id_for_metadata("123", None), "123");

    let reply_metadata = DiscordSendMetadata::with_reply_to_message_id(" origin-1 ");
    assert_eq!(reply_metadata.target_channel_id("123"), "123");
    assert_eq!(reply_metadata.reply_to_message_id(), Some("origin-1"));

    let combined = DiscordSendMetadata::with_thread_and_reply("thread-1", "origin-2");
    assert_eq!(combined.target_channel_id("123"), "thread-1");
    assert_eq!(combined.reply_to_message_id(), Some("origin-2"));

    let status_metadata =
        DiscordSendMetadata::with_thread_id("thread-2").with_non_conversational(true);
    assert_eq!(status_metadata.target_channel_id("123"), "thread-2");
    assert!(metadata_marks_non_conversational(Some(&status_metadata)));
}

#[test]
fn send_options_map_to_discord_metadata_without_losing_thread_or_status_flags() {
    let options = SendMessageOptions::non_conversational_threaded(Some(" thread-42 "));
    let metadata = discord_metadata_from_send_options(&options).expect("metadata");
    assert_eq!(metadata.target_channel_id("root"), "thread-42");
    assert!(metadata.marks_non_conversational());

    assert!(discord_metadata_from_send_options(&SendMessageOptions::default()).is_none());
}

#[test]
fn clarify_choice_normalization_unwraps_llm_dict_shapes_only() {
    let choices = discord_normalize_clarify_choices([
        serde_json::json!({"description": "Tight, well-illustrated"}),
        serde_json::json!({"name": "raw-name", "description": "Prefer description"}),
        serde_json::json!({"label": "Prefer label", "description": "description"}),
        serde_json::json!({"text": "Use text key"}),
        serde_json::json!({"title": "Use title key"}),
        serde_json::json!({"name": "do-not-use-name"}),
        serde_json::json!({"value": "do-not-use-value"}),
        serde_json::json!(["nested", {"description": "dict"}]),
        serde_json::json!(true),
        serde_json::Value::Null,
    ]);

    assert_eq!(
        choices,
        vec![
            "Tight, well-illustrated".to_string(),
            "Prefer description".to_string(),
            "Prefer label".to_string(),
            "Use text key".to_string(),
            "Use title key".to_string(),
            "nested dict".to_string(),
            "true".to_string(),
        ]
    );
}

#[test]
fn clarify_button_labels_fit_discord_cap_and_cut_at_boundaries() {
    let wordy = "Tight, well-illustrated, covers all 3 audiences (patients, families, curious general readers)";
    let label = discord_clarify_button_label(0, wordy);
    assert!(label.starts_with("1. "));
    assert!(label.ends_with('…'));
    assert!(label.chars().count() <= 80);
    assert!(!label.trim_end_matches('…').ends_with('('));

    let no_space = format!(
        "{}-{}-{}-{}",
        "a".repeat(30),
        "b".repeat(30),
        "c".repeat(30),
        "d".repeat(30)
    );
    let label = discord_clarify_button_label(0, &no_space);
    assert!(label.ends_with('…'));
    assert!(label.chars().count() <= 80);
    let body = label
        .strip_prefix("1. ")
        .expect("prefix")
        .trim_end_matches('…');
    assert!(matches!(
        body.chars().last(),
        Some('-' | ',' | '.' | ')' | ' ')
    ));
}

#[test]
fn allowed_mentions_safe_defaults_block_broad_pings() {
    let mentions = discord_allowed_mentions_from_lookup(|_| None);
    assert_eq!(mentions.parse, vec!["users".to_string()]);
    assert!(mentions.replied_user);

    let body = with_allowed_mentions(serde_json::json!({ "content": "hello" }), mentions);
    assert_eq!(
        body["allowed_mentions"],
        serde_json::json!({ "parse": ["users"], "replied_user": true })
    );
}

#[test]
fn reply_to_mode_defaults_to_first_and_parses_effective_behavior() {
    assert_eq!(default_reply_to_mode(), "first");
    assert_eq!(DiscordReplyToMode::parse(None), DiscordReplyToMode::First);
    assert_eq!(
        DiscordReplyToMode::parse(Some("")),
        DiscordReplyToMode::First
    );
    assert_eq!(
        DiscordReplyToMode::parse(Some("off")),
        DiscordReplyToMode::Off
    );
    assert_eq!(
        DiscordReplyToMode::parse(Some("ALL")),
        DiscordReplyToMode::All
    );
    assert_eq!(
        DiscordReplyToMode::parse(Some("banana")),
        DiscordReplyToMode::First
    );

    assert!(!DiscordReplyToMode::Off.references_chunk(0));
    assert!(DiscordReplyToMode::First.references_chunk(0));
    assert!(!DiscordReplyToMode::First.references_chunk(1));
    assert!(DiscordReplyToMode::All.references_chunk(0));
    assert!(DiscordReplyToMode::All.references_chunk(7));
}

#[test]
fn reply_reference_body_matches_discord_reference_contract() {
    let body = discord_message_body(
        "chunk",
        Some(" origin-1 "),
        DiscordAllowedMentions::from_flags(false, false, true, true),
    );

    assert_eq!(body["content"], "chunk");
    assert_eq!(
        body["allowed_mentions"],
        serde_json::json!({ "parse": ["users"], "replied_user": true })
    );
    assert_eq!(
        body["message_reference"],
        serde_json::json!({
            "message_id": "origin-1",
            "fail_if_not_exists": false
        })
    );

    let no_reference = discord_message_body(
        "chunk",
        None,
        DiscordAllowedMentions::from_flags(false, false, true, true),
    );
    assert!(no_reference.get("message_reference").is_none());
}

#[test]
fn reply_reference_retry_classifier_only_matches_reference_failures() {
    assert!(discord_reply_reference_error_allows_retry(
            "400 Bad Request (error code: 50035): Invalid Form Body\nIn message_reference: Cannot reply to a system message"
        ));
    assert!(discord_reply_reference_error_allows_retry(
        "400 Bad Request (error code: 10008): Unknown Message"
    ));
    assert!(!discord_reply_reference_error_allows_retry(
        "403 Forbidden (error code: 50013): Missing Permissions"
    ));
}

#[test]
fn forum_parent_and_payload_contract_matches_python_send_path() {
    assert!(!discord_channel_type_is_forum_parent(None));
    assert!(!discord_channel_type_is_forum_parent(Some(0)));
    assert!(!discord_channel_type_is_forum_parent(Some(11)));
    assert!(discord_channel_type_is_forum_parent(Some(15)));

    assert_eq!(
        forum_thread_name(Some("  here is a photo\nsecond line"), Some("photo.png")),
        "here is a photo"
    );
    assert_eq!(forum_thread_name(Some(""), Some("voice.ogg")), "voice.ogg");
    assert_eq!(forum_thread_name(None, None), "Hermes");

    let payload = forum_thread_payload("Hello forum!", None, Some(60));
    assert_eq!(payload["name"], "Hello forum!");
    assert_eq!(payload["auto_archive_duration"], 60);
    assert_eq!(payload["message"]["content"], "Hello forum!");
    assert_eq!(
        payload["message"]["allowed_mentions"],
        serde_json::json!({ "parse": ["users"], "replied_user": true })
    );
}

#[test]
fn allowed_mentions_env_style_knobs_parse_like_upstream() {
    let mentions = discord_allowed_mentions_from_lookup(|name| match name {
        DISCORD_ALLOW_MENTION_EVERYONE_ENV => Some(" true ".to_string()),
        DISCORD_ALLOW_MENTION_ROLES_ENV => Some("YES".to_string()),
        DISCORD_ALLOW_MENTION_USERS_ENV => Some("false".to_string()),
        DISCORD_ALLOW_MENTION_REPLIED_USER_ENV => Some("0".to_string()),
        _ => None,
    });

    assert_eq!(
        mentions,
        DiscordAllowedMentions::from_flags(true, true, false, false)
    );
}

#[test]
fn allowed_mentions_boolean_parser_falls_back_for_empty_or_unknown_values() {
    for raw in ["true", "True", "1", "yes", "on", " true "] {
        assert!(parse_allowed_mention_bool(raw, false));
    }
    for raw in ["false", "False", "0", "no", "off"] {
        assert!(!parse_allowed_mention_bool(raw, true));
    }

    assert!(!parse_allowed_mention_bool("", false));
    assert!(parse_allowed_mention_bool("", true));
    assert!(!parse_allowed_mention_bool("garbage", false));
    assert!(parse_allowed_mention_bool("garbage", true));
}

#[test]
fn bot_message_policy_defaults_to_none_and_parses_case_insensitively() {
    assert_eq!(
        DiscordBotMessagePolicy::parse(None),
        DiscordBotMessagePolicy::None
    );
    assert_eq!(
        DiscordBotMessagePolicy::parse(Some(" ALL ")),
        DiscordBotMessagePolicy::All
    );
    assert_eq!(
        DiscordBotMessagePolicy::parse(Some("mentions")),
        DiscordBotMessagePolicy::Mentions
    );
    assert_eq!(
        DiscordBotMessagePolicy::parse(Some("banana")),
        DiscordBotMessagePolicy::None
    );
    assert_eq!(
        DiscordBotMessagePolicy::from_lookup(|name| {
            (name == DISCORD_ALLOW_BOTS_ENV).then(|| "Mentions".to_string())
        }),
        DiscordBotMessagePolicy::Mentions
    );
    assert!(!DiscordBotMessagePolicy::None.bypasses_gateway_allowlist());
    assert!(DiscordBotMessagePolicy::Mentions.bypasses_gateway_allowlist());
    assert!(DiscordBotMessagePolicy::All.bypasses_gateway_allowlist());
}

#[test]
fn bot_message_filter_matches_upstream_contract() {
    let human = IncomingDiscordMessage {
        channel_id: "channel".into(),
        message_id: "message".into(),
        user_id: Some("human".into()),
        username: Some("Jezza".into()),
        content: "hello".into(),
        is_bot: false,
        message_type: 0,
        mention_user_ids: Vec::new(),
        reply_to_message_id: None,
        reply_to_text: None,
        attachments: Vec::new(),
    };
    assert!(DiscordAdapter::should_accept_message(
        &human,
        Some("self"),
        DiscordBotMessagePolicy::None
    ));

    let bot = IncomingDiscordMessage {
        is_bot: true,
        user_id: Some("bot".into()),
        username: Some("Worker".into()),
        mention_user_ids: vec!["self".into()],
        ..human.clone()
    };
    assert!(!DiscordAdapter::should_accept_message(
        &bot,
        Some("self"),
        DiscordBotMessagePolicy::None
    ));
    assert!(DiscordAdapter::should_accept_message(
        &bot,
        Some("self"),
        DiscordBotMessagePolicy::All
    ));
    assert!(DiscordAdapter::should_accept_message(
        &bot,
        Some("self"),
        DiscordBotMessagePolicy::Mentions
    ));

    let unmentioned_bot = IncomingDiscordMessage {
        mention_user_ids: vec!["someone-else".into()],
        ..bot.clone()
    };
    assert!(!DiscordAdapter::should_accept_message(
        &unmentioned_bot,
        Some("self"),
        DiscordBotMessagePolicy::Mentions
    ));

    let own_message = IncomingDiscordMessage {
        user_id: Some("self".into()),
        is_bot: true,
        ..bot
    };
    assert!(!DiscordAdapter::should_accept_message(
        &own_message,
        Some("self"),
        DiscordBotMessagePolicy::All
    ));
}

#[test]
fn system_message_filter_only_accepts_default_and_reply() {
    let mut msg = IncomingDiscordMessage {
        channel_id: "channel".into(),
        message_id: "message".into(),
        user_id: Some("human".into()),
        username: Some("Jezza".into()),
        content: "hello".into(),
        is_bot: false,
        message_type: 0,
        mention_user_ids: Vec::new(),
        reply_to_message_id: None,
        reply_to_text: None,
        attachments: Vec::new(),
    };
    assert!(DiscordAdapter::should_accept_message(
        &msg,
        Some("self"),
        DiscordBotMessagePolicy::None
    ));
    msg.message_type = 19;
    assert!(DiscordAdapter::should_accept_message(
        &msg,
        Some("self"),
        DiscordBotMessagePolicy::None
    ));
    for system_type in [1, 6, 7, 8] {
        msg.message_type = system_type;
        assert!(!DiscordAdapter::should_accept_message(
            &msg,
            Some("self"),
            DiscordBotMessagePolicy::None
        ));
    }
}

#[test]
fn discord_reactions_default_enabled_and_false_values_disable() {
    assert!(discord_reactions_enabled_from_raw(None));
    assert!(discord_reactions_enabled_from_raw(Some("")));
    assert!(discord_reactions_enabled_from_raw(Some("yes")));
    assert!(!discord_reactions_enabled_from_raw(Some("false")));
    assert!(!discord_reactions_enabled_from_raw(Some("0")));
    assert!(!discord_reactions_enabled_from_raw(Some("off")));
}

#[test]
fn discord_channel_controls_parse_csv_and_yaml_shapes() {
    let mut extra = std::collections::HashMap::new();
    extra.insert(
        "ignored_channels".into(),
        serde_json::json!("500, 600 ,700"),
    );
    extra.insert("no_thread_channels".into(), serde_json::json!(["800", 900]));
    extra.insert("free_response_channels".into(), serde_json::json!(1000));
    extra.insert("auto_thread".into(), serde_json::json!("false"));
    extra.insert("thread_require_mention".into(), serde_json::json!("yes"));

    let controls = DiscordChannelControls::from_extra(&extra);
    assert_eq!(
        controls.ignored_channels,
        ["500", "600", "700"]
            .into_iter()
            .map(String::from)
            .collect()
    );
    assert_eq!(
        controls.no_thread_channels,
        ["800", "900"].into_iter().map(String::from).collect()
    );
    assert_eq!(
        controls.free_response_channels,
        ["1000"].into_iter().map(String::from).collect()
    );
    assert!(!controls.auto_thread);
    assert!(controls.thread_require_mention);
}

#[test]
fn discord_channel_controls_ignore_server_channels_and_thread_parents() {
    let controls = DiscordChannelControls {
        ignored_channels: ["500"].into_iter().map(String::from).collect(),
        ..DiscordChannelControls::default()
    };

    assert!(controls.is_ignored(&DiscordChannelContext::server("500")));
    assert!(controls.is_ignored(&DiscordChannelContext::thread("501", "500")));
    assert!(!controls.is_ignored(&DiscordChannelContext::server("700")));
    assert!(!controls.is_ignored(&DiscordChannelContext::dm("500")));
}

#[test]
fn discord_channel_controls_auto_thread_policy_matches_upstream_cases() {
    let controls = DiscordChannelControls {
        no_thread_channels: ["800"].into_iter().map(String::from).collect(),
        free_response_channels: ["900"].into_iter().map(String::from).collect(),
        ..DiscordChannelControls::default()
    };

    assert!(!controls.should_auto_thread(&DiscordChannelContext::server("800")));
    assert!(!controls.should_auto_thread(&DiscordChannelContext::thread("801", "800")));
    assert!(!controls.should_auto_thread(&DiscordChannelContext::server("900")));
    assert!(!controls.should_auto_thread(&DiscordChannelContext::dm("700")));
    let mut reply = DiscordChannelContext::server("700");
    reply.is_reply = true;
    assert!(!controls.should_auto_thread(&reply));
    assert!(controls.should_auto_thread(&DiscordChannelContext::server("700")));

    let disabled = DiscordChannelControls {
        auto_thread: false,
        no_thread_channels: ["800"].into_iter().map(String::from).collect(),
        ..DiscordChannelControls::default()
    };
    assert!(!disabled.should_auto_thread(&DiscordChannelContext::server("700")));
    assert!(!disabled.should_auto_thread(&DiscordChannelContext::server("800")));
}

#[test]
fn discord_channel_controls_honor_wildcard_lists() {
    let ignored = DiscordChannelControls {
        ignored_channels: ["*"].into_iter().map(String::from).collect(),
        ..DiscordChannelControls::default()
    };
    assert!(ignored.is_ignored(&DiscordChannelContext::server("700")));
    assert!(ignored.is_ignored(&DiscordChannelContext::thread("701", "700")));
    assert!(!ignored.is_ignored(&DiscordChannelContext::dm("700")));

    let free = DiscordChannelControls {
        free_response_channels: ["*"].into_iter().map(String::from).collect(),
        ..DiscordChannelControls::default()
    };
    assert!(free.allows_free_response(&DiscordChannelContext::server("900")));
    assert!(free.allows_free_response(&DiscordChannelContext::thread("901", "900")));
    assert!(!free.should_auto_thread(&DiscordChannelContext::server("900")));
}

#[test]
fn discord_component_auth_user_or_role_matches_and_fails_closed() {
    let policy = DiscordInteractionAuthPolicy {
        allowed_user_ids: ["11111"].into_iter().map(String::from).collect(),
        allowed_role_ids: ["42"].into_iter().map(String::from).collect(),
        ..DiscordInteractionAuthPolicy::default()
    };

    assert!(policy.component_allows(&DiscordInteractionSubject::user("11111")));
    assert!(policy.component_allows(&DiscordInteractionSubject {
        user_id: Some("99999".into()),
        role_ids: ["42"].into_iter().map(String::from).collect(),
        role_guild_id: None,
    }));
    assert!(!policy.component_allows(&DiscordInteractionSubject {
        user_id: Some("99999".into()),
        role_ids: ["7"].into_iter().map(String::from).collect(),
        role_guild_id: None,
    }));
    assert!(!policy.component_allows(&DiscordInteractionSubject::default()));
    assert!(DiscordInteractionAuthPolicy::default()
        .component_allows(&DiscordInteractionSubject::default()));
}

#[test]
fn discord_component_auth_allows_approved_pairing_store_user() {
    let policy = DiscordInteractionAuthPolicy {
        allowed_user_ids: ["11111"].into_iter().map(String::from).collect(),
        ..DiscordInteractionAuthPolicy::default()
    };
    let pairing = PairingManager::new();
    pairing.approve("99999");

    assert!(discord_component_allows_with_pairing(
        &policy,
        &DiscordInteractionSubject::user("99999"),
        Some(&pairing)
    ));
    assert!(!discord_component_allows_with_pairing(
        &policy,
        &DiscordInteractionSubject::user("77777"),
        Some(&pairing)
    ));
    pairing.deny("99999");
    assert!(!discord_component_allows_with_pairing(
        &policy,
        &DiscordInteractionSubject::user("99999"),
        Some(&pairing)
    ));
}

#[test]
fn discord_slash_auth_matches_channel_and_identity_policy() {
    let policy = DiscordInteractionAuthPolicy {
        allowed_user_ids: ["100200300"].into_iter().map(String::from).collect(),
        allowed_channels: ["1111", "2222"].into_iter().map(String::from).collect(),
        ignored_channels: ["9999"].into_iter().map(String::from).collect(),
        ..DiscordInteractionAuthPolicy::default()
    };
    let subject = DiscordInteractionSubject::user("100200300");

    assert_eq!(
        policy.authorize_slash(
            &subject,
            Some(&DiscordChannelContext::server("1111")),
            Some("guild-1"),
            None,
        ),
        DiscordAuthDecision::Allow
    );
    assert_eq!(
        policy.authorize_slash(
            &subject,
            Some(&DiscordChannelContext::server("3333")),
            Some("guild-1"),
            None,
        ),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels)
    );
    assert_eq!(
        policy.authorize_slash(
            &subject,
            Some(&DiscordChannelContext::server("9999")),
            Some("guild-1"),
            None,
        ),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::IgnoredChannels)
    );
    assert_eq!(
        policy.authorize_slash(&subject, None, Some("guild-1"), None),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels)
    );
    let identity_only_policy = DiscordInteractionAuthPolicy {
        allowed_user_ids: ["100200300"].into_iter().map(String::from).collect(),
        ..DiscordInteractionAuthPolicy::default()
    };
    assert_eq!(
        identity_only_policy.authorize_slash(
            &DiscordInteractionSubject::user("other"),
            None,
            Some("guild-1"),
            None,
        ),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
    );
    assert_eq!(
        identity_only_policy.authorize_slash(&subject, None, Some("guild-1"), None),
        DiscordAuthDecision::Allow
    );
    assert_eq!(
        policy.authorize_slash(
            &DiscordInteractionSubject::user("other"),
            Some(&DiscordChannelContext::server("1111")),
            Some("guild-1"),
            None,
        ),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
    );
    assert_eq!(
        policy.authorize_slash(
            &DiscordInteractionSubject::default(),
            Some(&DiscordChannelContext::dm("dm-1")),
            None,
            None,
        ),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
    );
}

#[test]
fn discord_slash_auth_scopes_roles_to_origin_guild_or_dm_opt_in() {
    let policy = DiscordInteractionAuthPolicy {
        allowed_role_ids: ["5555"].into_iter().map(String::from).collect(),
        ..DiscordInteractionAuthPolicy::default()
    };
    let foreign_role = DiscordInteractionSubject::member("42", ["5555"], "guild-a");
    let in_scope_role = DiscordInteractionSubject::member("42", ["5555"], "guild-b");

    assert_eq!(
        policy.authorize_slash(
            &foreign_role,
            Some(&DiscordChannelContext::server("9999")),
            Some("guild-b"),
            None,
        ),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
    );
    assert_eq!(
        policy.authorize_slash(
            &in_scope_role,
            Some(&DiscordChannelContext::server("9999")),
            Some("guild-b"),
            None,
        ),
        DiscordAuthDecision::Allow
    );
    assert_eq!(
        policy.authorize_slash(
            &foreign_role,
            Some(&DiscordChannelContext::dm("dm-1")),
            None,
            None,
        ),
        DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
    );
    assert_eq!(
        policy.authorize_slash(
            &foreign_role,
            Some(&DiscordChannelContext::dm("dm-1")),
            None,
            Some("guild-a"),
        ),
        DiscordAuthDecision::Allow
    );
}

#[test]
fn discord_mention_policy_covers_free_response_and_participated_threads() {
    let controls = DiscordChannelControls {
        free_response_channels: ["222"].into_iter().map(String::from).collect(),
        ..DiscordChannelControls::default()
    };
    assert!(!discord_allows_message_without_mention(
        true,
        &controls,
        &DiscordChannelContext::server("111"),
        false,
        false,
    ));
    assert!(discord_allows_message_without_mention(
        true,
        &controls,
        &DiscordChannelContext::server("222"),
        false,
        false,
    ));
    assert!(discord_allows_message_without_mention(
        true,
        &controls,
        &DiscordChannelContext::thread("333", "222"),
        false,
        false,
    ));
    assert!(discord_allows_message_without_mention(
        true,
        &controls,
        &DiscordChannelContext::thread("444", "111"),
        true,
        false,
    ));
    let strict_threads = DiscordChannelControls {
        thread_require_mention: true,
        ..DiscordChannelControls::default()
    };
    assert!(!discord_allows_message_without_mention(
        true,
        &strict_threads,
        &DiscordChannelContext::thread("444", "111"),
        true,
        false,
    ));
    assert!(discord_allows_message_without_mention(
        true,
        &controls,
        &DiscordChannelContext::server("111"),
        false,
        true,
    ));
}

#[test]
fn discord_channel_context_fetch_gate_includes_reply_hydration() {
    let server = DiscordChannelContext::server("111");
    assert!(discord_should_fetch_channel_context(
        true, false, false, &server, false
    ));
    assert!(!discord_should_fetch_channel_context(
        false, true, false, &server, false
    ));
    assert!(discord_should_fetch_channel_context(
        false,
        true,
        false,
        &DiscordChannelContext::thread("222", "111"),
        false
    ));

    let mut reply = DiscordChannelContext::server("111");
    reply.is_reply = true;
    assert!(discord_should_fetch_channel_context(
        false, true, false, &reply, false
    ));
    assert!(!discord_should_fetch_channel_context(
        false, true, false, &reply, true
    ));
    assert!(!discord_should_fetch_channel_context(
        true,
        false,
        false,
        &DiscordChannelContext::dm("dm-1"),
        false
    ));
}

#[test]
fn discord_history_context_skips_status_messages_before_self_partition() {
    let non_conversational_ids = BTreeSet::from(["9".to_string()]);
    let primary = vec![
        DiscordHistoryMessage::self_message(
            "9",
            "arbitrary lifecycle text from a metadata-marked send",
        ),
        DiscordHistoryMessage::self_message(
            "8",
            "[Background process bg-123 finished with exit code 0~ Here's the final output:\nok]",
        ),
        DiscordHistoryMessage::bot_message(
            "7",
            "Codex",
            "♻ Gateway restarted successfully. Your session continues.",
        ),
        DiscordHistoryMessage::self_message("6", "💾 Self-improvement review: Memory updated"),
        DiscordHistoryMessage::new("5", "Alice", "question after reply"),
        DiscordHistoryMessage::self_message(
            "4",
            "💾 Self-improvement review: Skill 'hermes-gateway-display-config' patched",
        ),
        DiscordHistoryMessage::bot_message("3", "Codex", "Codex final answer"),
        DiscordHistoryMessage::new("2", "Alice", "prompt before reply"),
        DiscordHistoryMessage::self_message("1", "our prior response"),
    ];

    let result = discord_format_channel_context(&primary, &[], true, None, &non_conversational_ids);

    assert_eq!(
            result,
            "[Recent channel messages]\n[Alice] prompt before reply\n[Codex [bot]] Codex final answer\n[Alice] question after reply"
        );
    assert!(discord_looks_like_non_conversational_history_message(
        "💾 Self-improvement review: Memory updated"
    ));
    assert!(!discord_looks_like_non_conversational_history_message(
        "Self-improvement review: this is a normal assistant heading"
    ));
}

#[test]
fn discord_history_context_hydrates_reply_window_without_duplicates() {
    let primary = vec![
        DiscordHistoryMessage::new("6", "Alice", "latest note"),
        DiscordHistoryMessage::self_message("5", "our prior response"),
    ];
    let reply_window = vec![
        DiscordHistoryMessage::self_message("3", "the bot answer being replied to"),
        DiscordHistoryMessage::new("2", "Carol", "older question"),
        DiscordHistoryMessage::new("1", "Alice", "even older"),
    ];
    let result =
        discord_format_channel_context(&primary, &reply_window, true, Some("3"), &BTreeSet::new());

    assert!(result.contains("[Context around the replied-to message]"));
    assert!(result.contains("[Hermes [bot]] the bot answer being replied to"));
    assert!(result.contains("[Carol] older question"));
    assert!(result.contains("[Recent channel messages]"));
    assert!(result.contains("[Alice] latest note"));
    assert!(
        result.find("[Context around the replied-to message]")
            < result.find("[Recent channel messages]")
    );

    let primary_with_target = vec![
        DiscordHistoryMessage::new("4", "Alice", "recent reply target"),
        DiscordHistoryMessage::new("3", "Alice", "another recent"),
        DiscordHistoryMessage::self_message("2", "our prior response"),
    ];
    let duplicate = discord_format_channel_context(
        &primary_with_target,
        &reply_window,
        true,
        Some("4"),
        &BTreeSet::new(),
    );
    assert!(!duplicate.contains("[Context around the replied-to message]"));
    assert_eq!(duplicate.matches("recent reply target").count(), 1);
}

#[test]
fn discord_unauthorized_notify_soft_fail_falls_through() {
    assert!(!discord_notify_result_counts_delivered(Some(false)));
    assert!(discord_notify_result_counts_delivered(Some(true)));
    assert!(discord_notify_result_counts_delivered(None));
}

#[test]
fn discord_skill_slash_auth_gates_autocomplete_and_handler_before_lookup() {
    let policy = DiscordInteractionAuthPolicy {
        allowed_user_ids: ["100200300"].into_iter().map(String::from).collect(),
        ..DiscordInteractionAuthPolicy::default()
    };
    let entries = vec![
        DiscordSkillCommandEntry {
            name: "alpha".into(),
            description: "First skill".into(),
            command_key: "/alpha".into(),
        },
        DiscordSkillCommandEntry {
            name: "beta".into(),
            description: "Search documents".into(),
            command_key: "/beta".into(),
        },
    ];
    let channel = DiscordChannelContext::server("1111");

    let unauthorized = DiscordInteractionSubject::user("999999999");
    assert!(discord_skill_autocomplete_choices(
        &policy,
        &unauthorized,
        Some(&channel),
        Some("guild-1"),
        None,
        &entries,
        ""
    )
    .is_empty());
    assert_eq!(
        discord_skill_command_decision(
            &policy,
            &unauthorized,
            Some(&channel),
            Some("guild-1"),
            None,
            &entries,
            DiscordSkillCommandRequest {
                requested_name: "alpha",
                args: "extra",
            }
        ),
        DiscordSkillCommandDecision::Unauthorized
    );
    assert_eq!(
        discord_skill_command_decision(
            &policy,
            &unauthorized,
            Some(&channel),
            Some("guild-1"),
            None,
            &entries,
            DiscordSkillCommandRequest {
                requested_name: "definitely-not-a-skill",
                args: "",
            }
        ),
        DiscordSkillCommandDecision::Unauthorized
    );

    let authorized = DiscordInteractionSubject::user("100200300");
    assert_eq!(
        discord_skill_autocomplete_choices(
            &policy,
            &authorized,
            Some(&channel),
            Some("guild-1"),
            None,
            &entries,
            "doc"
        ),
        vec!["beta".to_string()]
    );
    assert_eq!(
        discord_skill_command_decision(
            &policy,
            &authorized,
            Some(&channel),
            Some("guild-1"),
            None,
            &entries,
            DiscordSkillCommandRequest {
                requested_name: "alpha",
                args: "extra args",
            }
        ),
        DiscordSkillCommandDecision::Dispatch {
            text: "/alpha extra args".into()
        }
    );
    assert_eq!(
        discord_skill_command_decision(
            &policy,
            &authorized,
            Some(&channel),
            Some("guild-1"),
            None,
            &entries,
            DiscordSkillCommandRequest {
                requested_name: "missing",
                args: "",
            }
        ),
        DiscordSkillCommandDecision::UnknownSkill {
            requested_name: "missing".into()
        }
    );
}

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

#[test]
fn media_methods_accept_metadata_contract() {
    let adapter = DiscordAdapter::new(test_config()).unwrap();
    let metadata = DiscordSendMetadata::with_thread_id("thread-1");

    let image_file = adapter.send_image_file(
        "channel-1",
        "/tmp/missing-image.png",
        Some("caption"),
        Some(&metadata),
    );
    drop(image_file);

    let image = adapter.send_image(
        "channel-1",
        "https://example.com/image.png",
        Some("caption"),
        Some(&metadata),
    );
    drop(image);

    let voice = adapter.send_voice(
        "channel-1",
        "/tmp/missing-audio.ogg",
        Some("caption"),
        Some(&metadata),
    );
    drop(voice);
}

#[test]
fn split_message_short() {
    let chunks = split_message("hello", 2000);
    assert_eq!(chunks, vec!["hello"]);
}

#[test]
fn split_message_long() {
    let text = "a".repeat(3000);
    let chunks = split_message(&text, 2000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 2000);
    assert_eq!(chunks[1].len(), 1000);
}

#[test]
fn gateway_payload_identify() {
    let adapter = DiscordAdapter::new(test_config()).unwrap();
    let payload = adapter.build_identify_payload();
    assert_eq!(payload.op, opcodes::IDENTIFY);
    assert!(payload.d.is_some());
}

#[test]
fn gateway_payload_heartbeat() {
    let payload = DiscordAdapter::build_heartbeat_payload(Some(42));
    assert_eq!(payload.op, opcodes::HEARTBEAT);
    assert_eq!(payload.d, Some(serde_json::Value::Number(42.into())));
}

#[test]
fn parse_message_create_event() {
    let data = serde_json::json!({
        "id": "msg123",
        "channel_id": "ch456",
        "content": "hello world",
        "type": 19,
        "mentions": [
            { "id": "bot-self", "username": "Hermes" }
        ],
        "attachments": [
            {
                "filename": "current.txt",
                "url": "https://cdn.discordapp.com/current.txt",
                "content_type": "text/plain",
                "size": 11
            }
        ],
        "message_reference": { "message_id": "origin-1" },
        "referenced_message": {
            "content": "original message",
            "attachments": [
                {
                    "filename": "image.png",
                    "url": "https://cdn.discordapp.com/attachments/image.png",
                    "content_type": "image/png",
                    "size": 1234
                }
            ]
        },
        "author": {
            "id": "user789",
            "username": "testuser",
            "bot": false
        }
    });

    let msg = DiscordAdapter::parse_message_create(&data).unwrap();
    assert_eq!(msg.channel_id, "ch456");
    assert_eq!(msg.message_id, "msg123");
    assert_eq!(msg.content, "hello world");
    assert_eq!(msg.user_id, Some("user789".into()));
    assert_eq!(msg.username, Some("testuser".into()));
    assert!(!msg.is_bot);
    assert_eq!(msg.message_type, 19);
    assert!(msg.mentions_user("bot-self"));
    assert_eq!(msg.reply_to_message_id.as_deref(), Some("origin-1"));
    assert_eq!(msg.reply_to_text.as_deref(), Some("original message"));
    assert_eq!(msg.attachments.len(), 2);
    assert_eq!(msg.attachments[0].filename, "current.txt");
    assert_eq!(msg.attachments[1].filename, "image.png");
    assert_eq!(
        msg.attachments[1].content_type.as_deref(),
        Some("image/png")
    );
}

#[test]
fn parse_message_create_bot() {
    let data = serde_json::json!({
        "id": "msg1",
        "channel_id": "ch1",
        "content": "bot msg",
        "author": { "id": "bot1", "username": "mybot", "bot": true }
    });

    let msg = DiscordAdapter::parse_message_create(&data).unwrap();
    assert!(msg.is_bot);
}

// -- GatewaySession tests -----------------------------------------------

#[test]
fn session_handles_hello() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::HELLO,
        d: Some(serde_json::json!({ "heartbeat_interval": 41250 })),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(session.heartbeat_interval_ms, Some(41250));
    assert!(actions.contains(&GatewayAction::SendHeartbeat));
    assert!(actions.contains(&GatewayAction::SendIdentify));
}

#[test]
fn session_handles_hello_with_resume() {
    let mut session = GatewaySession::new();
    session.session_id = Some("sess123".into());
    session.sequence = Some(42);

    let payload = GatewayPayload {
        op: opcodes::HELLO,
        d: Some(serde_json::json!({ "heartbeat_interval": 30000 })),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert!(actions.contains(&GatewayAction::SendResume));
    assert!(!actions.contains(&GatewayAction::SendIdentify));
}

#[test]
fn session_handles_heartbeat_ack() {
    let mut session = GatewaySession::new();
    session.heartbeat_acknowledged = false;

    let payload = GatewayPayload {
        op: opcodes::HEARTBEAT_ACK,
        d: None,
        s: None,
        t: None,
    };

    session.handle_gateway_event(&payload);
    assert!(session.heartbeat_acknowledged);
}

#[test]
fn session_handles_reconnect() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::RECONNECT,
        d: None,
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::Reconnect]);
}

#[test]
fn session_handles_invalid_session_resumable() {
    let mut session = GatewaySession::new();
    session.session_id = Some("sess".into());
    session.sequence = Some(10);

    let payload = GatewayPayload {
        op: opcodes::INVALID_SESSION,
        d: Some(serde_json::Value::Bool(true)),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::InvalidSession(true)]);
    assert!(session.session_id.is_some());
}

#[test]
fn session_handles_invalid_session_not_resumable() {
    let mut session = GatewaySession::new();
    session.session_id = Some("sess".into());
    session.sequence = Some(10);

    let payload = GatewayPayload {
        op: opcodes::INVALID_SESSION,
        d: Some(serde_json::Value::Bool(false)),
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::InvalidSession(false)]);
    assert!(session.session_id.is_none());
    assert!(session.sequence.is_none());
}

#[test]
fn session_handles_ready_dispatch() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::DISPATCH,
        d: Some(serde_json::json!({
            "session_id": "abc123",
            "resume_gateway_url": "wss://resume.discord.gg",
            "user": { "id": "12345", "username": "testbot" }
        })),
        s: Some(1),
        t: Some("READY".into()),
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(session.session_id, Some("abc123".into()));
    assert_eq!(
        session.resume_gateway_url,
        Some("wss://resume.discord.gg".into())
    );
    assert_eq!(session.sequence, Some(1));
    assert!(session.identified);

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        GatewayAction::Dispatch(name, _) => assert_eq!(name, "READY"),
        other => panic!("expected Dispatch, got {:?}", other),
    }
}

#[test]
fn session_tracks_sequence() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::DISPATCH,
        d: Some(serde_json::json!({})),
        s: Some(42),
        t: Some("GUILD_CREATE".into()),
    };

    session.handle_gateway_event(&payload);
    assert_eq!(session.sequence, Some(42));
}

#[test]
fn session_zombie_detection() {
    let mut session = GatewaySession::new();
    assert!(!session.is_zombie());

    session.heartbeat_sent();
    assert!(session.is_zombie());

    session.heartbeat_acknowledged = true;
    assert!(!session.is_zombie());
}

#[test]
fn session_reset() {
    let mut session = GatewaySession::new();
    session.session_id = Some("s".into());
    session.sequence = Some(99);
    session.heartbeat_interval_ms = Some(5000);
    session.identified = true;

    session.reset();
    assert!(session.session_id.is_none());
    assert!(session.sequence.is_none());
    assert!(session.heartbeat_interval_ms.is_none());
    assert!(!session.identified);
}

#[test]
fn session_heartbeat_request() {
    let mut session = GatewaySession::new();
    let payload = GatewayPayload {
        op: opcodes::HEARTBEAT,
        d: None,
        s: None,
        t: None,
    };

    let actions = session.handle_gateway_event(&payload);
    assert_eq!(actions, vec![GatewayAction::SendHeartbeat]);
}

// -- Event parsing tests ------------------------------------------------

#[test]
fn parse_message_update_full() {
    let data = serde_json::json!({
        "id": "msg100",
        "channel_id": "ch200",
        "content": "edited content",
        "author": { "id": "user300" },
        "guild_id": "guild400"
    });

    let evt = DiscordAdapter::parse_message_update(&data).unwrap();
    assert_eq!(evt.message_id, "msg100");
    assert_eq!(evt.channel_id, "ch200");
    assert_eq!(evt.content, Some("edited content".into()));
    assert_eq!(evt.author_id, Some("user300".into()));
    assert_eq!(evt.guild_id, Some("guild400".into()));
}

#[test]
fn parse_message_update_partial() {
    let data = serde_json::json!({
        "id": "msg100",
        "channel_id": "ch200"
    });

    let evt = DiscordAdapter::parse_message_update(&data).unwrap();
    assert!(evt.content.is_none());
    assert!(evt.author_id.is_none());
}

#[test]
fn parse_interaction_create_slash_command() {
    let data = serde_json::json!({
        "id": "int1",
        "application_id": "app1",
        "type": 2,
        "token": "tok1",
        "channel_id": "ch1",
        "guild_id": "g1",
        "member": {
            "user": { "id": "u1" }
        },
        "data": {
            "name": "hello",
            "options": [
                { "name": "target", "value": "world" },
                { "name": "count", "value": 3 }
            ]
        }
    });

    let interaction = DiscordAdapter::parse_interaction_create(&data).unwrap();
    assert_eq!(interaction.id, "int1");
    assert_eq!(interaction.interaction_type, 2);
    assert_eq!(interaction.command_name, Some("hello".into()));
    assert_eq!(interaction.user_id, Some("u1".into()));
    assert_eq!(interaction.command_options.len(), 2);
    assert_eq!(interaction.command_options[0].name, "target");
    assert_eq!(
        interaction.command_options[0].value,
        serde_json::json!("world")
    );
    assert_eq!(interaction.command_options[1].name, "count");
    assert_eq!(interaction.command_options[1].value, serde_json::json!(3));
}

#[test]
fn parse_interaction_create_dm() {
    let data = serde_json::json!({
        "id": "int2",
        "application_id": "app2",
        "type": 2,
        "token": "tok2",
        "user": { "id": "dm_user" },
        "data": { "name": "ping" }
    });

    let interaction = DiscordAdapter::parse_interaction_create(&data).unwrap();
    assert_eq!(interaction.user_id, Some("dm_user".into()));
    assert!(interaction.guild_id.is_none());
    assert!(interaction.command_options.is_empty());
}

#[test]
fn parse_reaction_add_event() {
    let data = serde_json::json!({
        "user_id": "u1",
        "channel_id": "ch1",
        "message_id": "msg1",
        "guild_id": "g1",
        "emoji": {
            "name": "\u{1f44d}",
            "id": null
        }
    });

    let evt = DiscordAdapter::parse_reaction_event(&data).unwrap();
    assert_eq!(evt.user_id, "u1");
    assert_eq!(evt.channel_id, "ch1");
    assert_eq!(evt.message_id, "msg1");
    assert_eq!(evt.guild_id, Some("g1".into()));
    assert_eq!(evt.emoji_name, Some("\u{1f44d}".into()));
    assert!(evt.emoji_id.is_none());
}

#[test]
fn parse_reaction_custom_emoji() {
    let data = serde_json::json!({
        "user_id": "u2",
        "channel_id": "ch2",
        "message_id": "msg2",
        "emoji": {
            "name": "custom_emote",
            "id": "12345678"
        }
    });

    let evt = DiscordAdapter::parse_reaction_event(&data).unwrap();
    assert_eq!(evt.emoji_name, Some("custom_emote".into()));
    assert_eq!(evt.emoji_id, Some("12345678".into()));
}

#[test]
fn parse_voice_state_update_event() {
    let data = serde_json::json!({
        "guild_id": "g1",
        "channel_id": "vc1",
        "user_id": "u1",
        "session_id": "sess1",
        "deaf": false,
        "mute": false,
        "self_deaf": true,
        "self_mute": true,
        "suppress": false
    });

    let vs = DiscordAdapter::parse_voice_state_update(&data).unwrap();
    assert_eq!(vs.guild_id, Some("g1".into()));
    assert_eq!(vs.channel_id, Some("vc1".into()));
    assert_eq!(vs.user_id, "u1");
    assert!(!vs.deaf);
    assert!(!vs.mute);
    assert!(vs.self_deaf);
    assert!(vs.self_mute);
    assert!(!vs.suppress);
}

#[test]
fn parse_voice_state_leave() {
    let data = serde_json::json!({
        "guild_id": "g1",
        "channel_id": null,
        "user_id": "u1",
        "session_id": "sess2",
        "deaf": false,
        "mute": false,
        "self_deaf": false,
        "self_mute": false,
        "suppress": false
    });

    let vs = DiscordAdapter::parse_voice_state_update(&data).unwrap();
    assert!(vs.channel_id.is_none());
}

// -- Dispatch routing tests ---------------------------------------------

#[test]
fn dispatch_routes_message_create() {
    let data = serde_json::json!({
        "id": "m1",
        "channel_id": "c1",
        "content": "hi",
        "author": { "id": "u1", "username": "a", "bot": false }
    });

    let evt = DiscordAdapter::parse_dispatch("MESSAGE_CREATE", &data);
    assert!(matches!(evt, Some(DispatchEvent::MessageCreate(_))));
}

#[test]
fn dispatch_routes_message_update() {
    let data = serde_json::json!({ "id": "m1", "channel_id": "c1" });
    let evt = DiscordAdapter::parse_dispatch("MESSAGE_UPDATE", &data);
    assert!(matches!(evt, Some(DispatchEvent::MessageUpdate(_))));
}

#[test]
fn dispatch_routes_interaction_create() {
    let data = serde_json::json!({
        "id": "i1",
        "application_id": "a1",
        "type": 2,
        "token": "t1",
        "data": { "name": "test" }
    });
    let evt = DiscordAdapter::parse_dispatch("INTERACTION_CREATE", &data);
    assert!(matches!(evt, Some(DispatchEvent::InteractionCreate(_))));
}

#[test]
fn dispatch_routes_reaction_add() {
    let data = serde_json::json!({
        "user_id": "u1",
        "channel_id": "c1",
        "message_id": "m1",
        "emoji": { "name": "x" }
    });
    let evt = DiscordAdapter::parse_dispatch("MESSAGE_REACTION_ADD", &data);
    assert!(matches!(evt, Some(DispatchEvent::ReactionAdd(_))));
}

#[test]
fn dispatch_routes_reaction_remove() {
    let data = serde_json::json!({
        "user_id": "u1",
        "channel_id": "c1",
        "message_id": "m1",
        "emoji": { "name": "x" }
    });
    let evt = DiscordAdapter::parse_dispatch("MESSAGE_REACTION_REMOVE", &data);
    assert!(matches!(evt, Some(DispatchEvent::ReactionRemove(_))));
}

#[test]
fn dispatch_routes_voice_state() {
    let data = serde_json::json!({
        "user_id": "u1",
        "session_id": "s1",
        "deaf": false,
        "mute": false,
        "self_deaf": false,
        "self_mute": false,
        "suppress": false
    });
    let evt = DiscordAdapter::parse_dispatch("VOICE_STATE_UPDATE", &data);
    assert!(matches!(evt, Some(DispatchEvent::VoiceStateUpdate(_))));
}

#[test]
fn dispatch_unknown_event_returns_none() {
    let data = serde_json::json!({});
    let evt = DiscordAdapter::parse_dispatch("UNKNOWN_EVENT", &data);
    assert!(evt.is_none());
}

// -- Embed builder tests ------------------------------------------------

#[test]
fn embed_builder() {
    let embed = DiscordEmbed::new()
        .with_title("Test Embed")
        .with_description("A description")
        .with_color(0xFF5733)
        .with_footer("footer text")
        .with_timestamp("2026-01-01T00:00:00Z")
        .add_field("Field 1", "Value 1", true)
        .add_field("Field 2", "Value 2", false);

    assert_eq!(embed.title, Some("Test Embed".into()));
    assert_eq!(embed.description, Some("A description".into()));
    assert_eq!(embed.color, Some(0xFF5733));
    assert_eq!(embed.footer.as_ref().unwrap().text, "footer text");
    assert_eq!(embed.timestamp, Some("2026-01-01T00:00:00Z".into()));
    assert_eq!(embed.fields.len(), 2);
    assert_eq!(embed.fields[0].name, "Field 1");
    assert_eq!(embed.fields[0].inline, Some(true));
    assert_eq!(embed.fields[1].inline, Some(false));
}

#[test]
fn embed_serialization() {
    let embed = DiscordEmbed::new().with_title("Hello").with_color(0x00FF00);

    let json = serde_json::to_value(&embed).unwrap();
    assert_eq!(json["title"], "Hello");
    assert_eq!(json["color"], 0x00FF00);
    assert!(json.get("description").is_none());
    assert!(json.get("footer").is_none());
}

// -- Slash command serialization tests ----------------------------------

#[test]
fn slash_command_serialization() {
    let cmd = SlashCommand {
        name: "greet".into(),
        description: "Say hello".into(),
        default_member_permissions: None,
        dm_permission: None,
        nsfw: None,
        contexts: None,
        integration_types: None,
        command_type: 1,
        options: Some(vec![
            SlashCommandOption {
                name: "name".into(),
                description: "Who to greet".into(),
                option_type: 3, // STRING
                required: Some(true),
                choices: None,
            },
            SlashCommandOption {
                name: "style".into(),
                description: "Greeting style".into(),
                option_type: 3,
                required: Some(false),
                choices: Some(vec![
                    SlashCommandChoice {
                        name: "Formal".into(),
                        value: serde_json::json!("formal"),
                    },
                    SlashCommandChoice {
                        name: "Casual".into(),
                        value: serde_json::json!("casual"),
                    },
                ]),
            },
        ]),
    };

    let json = serde_json::to_value(&cmd).unwrap();
    assert_eq!(json["name"], "greet");
    assert_eq!(json["type"], 1);
    let options = json["options"].as_array().unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["required"], true);
    let choices = options[1]["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0]["name"], "Formal");
}

#[test]
fn slash_owner_only_visibility_sets_zero_permissions() {
    let mut commands = vec![SlashCommand {
        name: "restart".into(),
        description: "Restart Hermes".into(),
        options: None,
        default_member_permissions: None,
        dm_permission: None,
        nsfw: None,
        contexts: None,
        integration_types: None,
        command_type: 1,
    }];

    apply_owner_only_slash_visibility(&mut commands);

    assert_eq!(commands[0].default_member_permissions.as_deref(), Some("0"));
    let json = serde_json::to_value(&commands[0]).unwrap();
    assert_eq!(json["default_member_permissions"], "0");
}

// -- Emoji encoding tests -----------------------------------------------

#[test]
fn encode_emoji_unicode() {
    let encoded = encode_emoji("\u{1f44d}");
    assert_eq!(encoded, "%F0%9F%91%8D");
}

#[test]
fn encode_emoji_custom() {
    let encoded = encode_emoji("custom_emote:12345");
    assert_eq!(encoded, "custom_emote:12345");
}

// -- Default trait impls ------------------------------------------------

#[test]
fn gateway_session_default() {
    let session = GatewaySession::default();
    assert!(session.sequence.is_none());
    assert!(session.session_id.is_none());
    assert!(!session.identified);
    assert!(session.heartbeat_acknowledged);
}

#[test]
fn embed_default() {
    let embed = DiscordEmbed::default();
    assert!(embed.title.is_none());
    assert!(embed.fields.is_empty());
}
