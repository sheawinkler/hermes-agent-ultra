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
fn discord_outgoing_formatter_rewrites_gfm_tables_to_bullet_groups() {
    let formatted = format_discord_outgoing_content(
        "Report\n| Name | Score |\n| --- | ---: |\n| Ada | 10 |\n| Turing | 9 |",
    );

    assert_eq!(
        formatted,
        "Report\n**Ada**\n- Score: 10\n\n**Turing**\n- Score: 9"
    );
}

#[test]
fn discord_outgoing_formatter_preserves_fenced_tables_and_row_labels() {
    let fenced = format_discord_outgoing_content("```\n| A | B |\n|---|---|\n| 1 | 2 |\n```");
    assert_eq!(fenced, "```\n| A | B |\n|---|---|\n| 1 | 2 |\n```");

    let row_labels = format_discord_outgoing_content(
        "| Metric | Before | After |\n| --- | --- | --- |\n| p95 | latency | 120ms | 80ms |",
    );
    assert_eq!(
        row_labels,
        "**p95**\n- Metric: latency\n- Before: 120ms\n- After: 80ms"
    );
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
fn discord_allowed_user_wildcard_allows_component_and_slash_subjects() {
    let policy = DiscordInteractionAuthPolicy {
        allowed_user_ids: ["*"].into_iter().map(String::from).collect(),
        ..DiscordInteractionAuthPolicy::default()
    };
    let subject = DiscordInteractionSubject::user("any-user");

    assert!(policy.component_allows(&subject));
    assert_eq!(
        policy.authorize_slash(
            &subject,
            Some(&DiscordChannelContext::server("1111")),
            Some("guild-1"),
            None,
        ),
        DiscordAuthDecision::Allow
    );
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
