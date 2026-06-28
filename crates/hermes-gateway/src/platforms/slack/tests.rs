use super::*;
use std::sync::{Mutex, MutexGuard};
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

fn slack_test_config() -> SlackConfig {
    SlackConfig {
        token: "xoxb-test".into(),
        app_token: None,
        socket_mode: false,
        reactions: true,
        require_mention: false,
        bot_user_id: None,
        mention_patterns: Vec::new(),
        proxy: AdapterProxyConfig::default(),
    }
}

// --- Original tests (preserved) ---

#[test]
fn split_message_short() {
    let chunks = split_message("hello", 4000);
    assert_eq!(chunks, vec!["hello"]);
}

#[test]
fn split_message_long() {
    let text = "a".repeat(5000);
    let chunks = split_message(&text, 4000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 4000);
    assert_eq!(chunks[1].len(), 1000);
}

#[test]
fn parse_event_message() {
    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": { "type": "message", "text": "hello bot", "channel": "C123", "user": "U456", "ts": "1.0" }
        })),
    };
    let msg = SlackAdapter::parse_event(&env).unwrap();
    assert_eq!(msg.channel, "C123");
    assert_eq!(msg.user_id, Some("U456".into()));
    assert_eq!(msg.text, "hello bot");
    assert!(!msg.is_bot);
    assert!(msg.media_files.is_empty());
}

#[test]
fn slack_audio_ext_resolution_preserves_container_extensions() {
    assert_eq!(
        resolve_slack_audio_ext(Some("audio_message.mp4"), Some("audio/mp4")),
        ".mp4"
    );
    assert_eq!(
        resolve_slack_audio_ext(Some("voice.ogg"), Some("audio/ogg")),
        ".ogg"
    );
    assert_eq!(
        resolve_slack_audio_ext(Some("clip.m4a"), Some("audio/x-m4a")),
        ".m4a"
    );
    assert_eq!(resolve_slack_audio_ext(Some(""), Some("audio/mp4")), ".m4a");
    assert_eq!(
        resolve_slack_audio_ext(Some("weird"), Some("audio/x-future-codec")),
        ".m4a"
    );
}

#[test]
fn slack_voice_clip_detection_uses_stable_slack_markers() {
    assert!(slack_file_is_voice_clip(Some("audio_message.mp4"), None));
    assert!(slack_file_is_voice_clip(
        Some("clip.mp4"),
        Some("slack_audio")
    ));
    assert!(!slack_file_is_voice_clip(Some("vacation.mp4"), None));
    assert!(!slack_file_is_voice_clip(
        Some("screen_recording.mp4"),
        Some("slack_video")
    ));
}

#[test]
fn parse_event_preserves_audio_mp4_voice_attachment_metadata() {
    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "",
                "channel": "C123",
                "user": "U456",
                "ts": "1.0",
                "files": [{
                    "id": "F1",
                    "name": "audio_message.mp4",
                    "mimetype": "audio/mp4",
                    "url_private": "https://files.slack.test/F1",
                    "url_private_download": "https://files.slack.test/F1/download"
                }]
            }
        })),
    };

    let msg = SlackAdapter::parse_event(&env).unwrap();
    assert_eq!(msg.media_files.len(), 1);
    let file = &msg.media_files[0];
    assert_eq!(file.kind, SlackMediaKind::Audio);
    assert_eq!(file.cache_extension.as_deref(), Some(".mp4"));
    assert_eq!(file.reported_mime_type.as_deref(), Some("audio/mp4"));
    assert_eq!(
        file.download_url(),
        Some("https://files.slack.test/F1/download")
    );
}

#[test]
fn parse_event_reroutes_video_mp4_slack_voice_clip_to_audio() {
    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "",
                "channel": "C123",
                "user": "U456",
                "ts": "1.0",
                "files": [{
                    "id": "F2",
                    "name": "voice.wav",
                    "subtype": "slack_audio",
                    "mimetype": "video/mp4",
                    "url_private": "https://files.slack.test/F2"
                }]
            }
        })),
    };

    let msg = SlackAdapter::parse_event(&env).unwrap();
    let file = &msg.media_files[0];
    assert_eq!(file.kind, SlackMediaKind::Audio);
    assert_eq!(file.cache_extension.as_deref(), Some(".wav"));
    assert_eq!(file.reported_mime_type.as_deref(), Some("audio/wav"));
}

#[test]
fn parse_event_keeps_real_slack_video_on_video_path() {
    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "watch this",
                "channel": "C123",
                "user": "U456",
                "ts": "1.0",
                "files": [{
                    "id": "F3",
                    "name": "vacation.mp4",
                    "subtype": "slack_video",
                    "mimetype": "video/mp4",
                    "url_private": "https://files.slack.test/F3"
                }]
            }
        })),
    };

    let msg = SlackAdapter::parse_event(&env).unwrap();
    let file = &msg.media_files[0];
    assert_eq!(file.kind, SlackMediaKind::Video);
    assert_eq!(file.cache_extension, None);
    assert_eq!(file.reported_mime_type.as_deref(), Some("video/mp4"));
}

#[test]
fn parse_event_bot_message_skipped() {
    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": { "type": "message", "text": "bot msg", "channel": "C123", "bot_id": "B789", "ts": "1.0" }
        })),
    };
    assert!(SlackAdapter::parse_event(&env).is_none());
}

#[test]
fn parse_event_thread_reply() {
    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": { "type": "message", "text": "reply", "channel": "C1", "user": "U4",
                       "ts": "2.0", "thread_ts": "1.0" }
        })),
    };
    assert_eq!(
        SlackAdapter::parse_event(&env).unwrap().thread_ts,
        Some("1.0".into())
    );
}

#[test]
fn parse_event_non_message_skipped() {
    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": { "type": "reaction_added", "reaction": "thumbsup", "user": "U456" }
        })),
    };
    assert!(SlackAdapter::parse_event(&env).is_none());
}

#[test]
fn slack_mention_patterns_parse_json_string_and_csv_newlines() {
    assert_eq!(
        parse_slack_mention_pattern_values(r#"["^\\s*chompy\\b","@hermes"]"#),
        vec![r"^\s*chompy\b", "@hermes"]
    );
    assert_eq!(
        parse_slack_mention_pattern_values("chompy\\b\n@hermes, sigma"),
        vec!["chompy\\b", "@hermes", "sigma"]
    );
    assert_eq!(
        parse_slack_mention_pattern_values(r#""hey hermes""#),
        vec!["hey hermes"]
    );
}

#[test]
fn slack_mention_patterns_env_fallback_splits_mixed_csv_and_newlines() {
    let _env = env_lock();
    unsafe {
        std::env::set_var("SLACK_MENTION_PATTERNS", "chompy\\b\n@hermes, sigma");
    }
    assert!(slack_message_matches_mention_patterns(
        "SIGMA status",
        &Vec::new()
    ));
    assert!(slack_message_matches_mention_patterns(
        "hey @Hermes",
        &Vec::new()
    ));
    assert!(!slack_message_matches_mention_patterns(
        "plain channel chatter",
        &Vec::new()
    ));
    unsafe {
        std::env::remove_var("SLACK_MENTION_PATTERNS");
    }
}

#[test]
fn parse_event_with_config_requires_mention_but_accepts_wake_word() {
    let mut cfg = slack_test_config();
    cfg.require_mention = true;
    cfg.bot_user_id = Some("UBOT".into());
    cfg.mention_patterns = vec![r"^\s*chompy\b".into()];

    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "Chompy check gateway",
                "channel": "C123",
                "channel_type": "channel",
                "user": "U456",
                "ts": "1.0"
            }
        })),
    };

    let msg = SlackAdapter::parse_event_with_config(&env, &cfg).unwrap();
    assert_eq!(msg.text, "Chompy check gateway");
}

#[test]
fn parse_event_with_config_blocks_unaddressed_channel_but_allows_dm() {
    let mut cfg = slack_test_config();
    cfg.require_mention = true;
    cfg.bot_user_id = Some("UBOT".into());

    let channel_env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "general channel chatter",
                "channel": "C123",
                "channel_type": "channel",
                "user": "U456",
                "ts": "1.0"
            }
        })),
    };
    assert!(SlackAdapter::parse_event_with_config(&channel_env, &cfg).is_none());

    let dm_env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env124".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "dm chatter",
                "channel": "D123",
                "channel_type": "im",
                "user": "U456",
                "ts": "2.0"
            }
        })),
    };
    assert!(SlackAdapter::parse_event_with_config(&dm_env, &cfg).is_some());
}

#[test]
fn parse_event_with_config_accepts_literal_bot_mention() {
    let mut cfg = slack_test_config();
    cfg.require_mention = true;
    cfg.bot_user_id = Some("UBOT".into());

    let env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("env123".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "<@UBOT> status",
                "channel": "C123",
                "channel_type": "channel",
                "user": "U456",
                "ts": "1.0"
            }
        })),
    };
    assert!(SlackAdapter::parse_event_with_config(&env, &cfg).is_some());
}

// --- Socket Mode session ---

#[test]
fn socket_mode_session_lifecycle() {
    let mut session = SocketModeSession::new();
    assert_eq!(session.state(), SocketModeConnectionState::Disconnected);
    assert_eq!(session.envelopes_acked(), 0);

    session.mark_connecting();
    assert_eq!(session.state(), SocketModeConnectionState::Connecting);
    session.mark_connected();
    assert_eq!(session.state(), SocketModeConnectionState::Connected);
    session.mark_closing();
    assert_eq!(session.state(), SocketModeConnectionState::Closing);

    assert_eq!(
        SocketModeSession::default().state(),
        SocketModeConnectionState::Disconnected
    );
}

#[test]
fn build_ack_payload_format() {
    assert_eq!(
        SocketModeSession::build_ack_payload("abc-123"),
        r#"{"envelope_id":"abc-123"}"#
    );
}

#[test]
fn handle_envelope_hello_and_disconnect() {
    let mut session = SocketModeSession::new();
    let hello = SocketModeEnvelope {
        envelope_type: "hello".into(),
        envelope_id: None,
        payload: None,
    };
    assert_eq!(session.handle_envelope(&hello), SocketModeAction::Ignore);
    assert_eq!(session.state(), SocketModeConnectionState::Connected);

    let disc = SocketModeEnvelope {
        envelope_type: "disconnect".into(),
        envelope_id: None,
        payload: None,
    };
    assert_eq!(session.handle_envelope(&disc), SocketModeAction::Ignore);
    assert_eq!(session.state(), SocketModeConnectionState::Closing);
}

#[test]
fn handle_envelope_events_api() {
    let mut session = SocketModeSession::new();
    session.mark_connected();
    let msg_env = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("e1".into()),
        payload: Some(serde_json::json!({
            "event": { "type": "message", "text": "hi", "channel": "C9", "user": "UA", "ts": "1.2" }
        })),
    };
    match session.handle_envelope(&msg_env) {
        SocketModeAction::MessageEvent(m) => {
            assert_eq!(m.channel, "C9");
            assert_eq!(m.text, "hi");
        }
        other => panic!("Expected MessageEvent, got {:?}", other),
    }
    let non_msg = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("e2".into()),
        payload: Some(serde_json::json!({
            "event": { "type": "app_mention", "channel": "C1", "user": "U1", "ts": "1.0" }
        })),
    };
    assert_eq!(session.handle_envelope(&non_msg), SocketModeAction::Ack);
    assert_eq!(session.envelopes_acked(), 2);
}

#[test]
fn handle_envelope_events_api_respects_mention_policy() {
    let mut cfg = slack_test_config();
    cfg.require_mention = true;
    cfg.mention_patterns = vec![r"^\s*chompy\b".into()];
    let mut session = SocketModeSession::with_config(&cfg);

    let unaddressed = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("e1".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "general chatter",
                "channel": "C9",
                "channel_type": "channel",
                "user": "UA",
                "ts": "1.2"
            }
        })),
    };
    assert_eq!(session.handle_envelope(&unaddressed), SocketModeAction::Ack);

    let addressed = SocketModeEnvelope {
        envelope_type: "events_api".into(),
        envelope_id: Some("e2".into()),
        payload: Some(serde_json::json!({
            "event": {
                "type": "message",
                "text": "Chompy status",
                "channel": "C9",
                "channel_type": "channel",
                "user": "UA",
                "ts": "1.3"
            }
        })),
    };
    match session.handle_envelope(&addressed) {
        SocketModeAction::MessageEvent(m) => assert_eq!(m.text, "Chompy status"),
        other => panic!("Expected MessageEvent, got {:?}", other),
    }
    assert_eq!(session.envelopes_acked(), 2);
}

#[test]
fn handle_envelope_interactive() {
    let mut session = SocketModeSession::new();
    let envelope = SocketModeEnvelope {
        envelope_type: "interactive".into(),
        envelope_id: Some("e3".into()),
        payload: Some(serde_json::json!({
            "type": "block_actions", "trigger_id": "t1",
            "actions": [{ "action_id": "btn", "type": "button", "value": "ok" }],
            "user": { "id": "U1" }
        })),
    };
    match session.handle_envelope(&envelope) {
        SocketModeAction::InteractiveEvent(p) => {
            assert_eq!(p.payload_type, "block_actions");
            assert_eq!(p.actions[0].action_id.as_deref(), Some("btn"));
        }
        other => panic!("Expected InteractiveEvent, got {:?}", other),
    }
}

#[test]
fn handle_envelope_slash_command() {
    let mut session = SocketModeSession::new();
    let envelope = SocketModeEnvelope {
        envelope_type: "slash_commands".into(),
        envelope_id: Some("e4".into()),
        payload: Some(serde_json::json!({
            "command": "/deploy", "text": "prod", "channel_id": "C5", "user_id": "U7"
        })),
    };
    match session.handle_envelope(&envelope) {
        SocketModeAction::SlashCommand(cmd) => {
            assert_eq!(cmd.command, "/deploy");
            assert_eq!(cmd.text.as_deref(), Some("prod"));
        }
        other => panic!("Expected SlashCommand, got {:?}", other),
    }
}

#[test]
fn handle_envelope_unknown_ignored() {
    let mut s = SocketModeSession::new();
    let e = SocketModeEnvelope {
        envelope_type: "future".into(),
        envelope_id: None,
        payload: None,
    };
    assert_eq!(s.handle_envelope(&e), SocketModeAction::Ignore);
    assert_eq!(s.envelopes_acked(), 0);
}

// --- Interactive & slash command parsing ---

#[test]
fn interactive_payload_parsing() {
    let env = SocketModeEnvelope {
        envelope_type: "interactive".into(),
        envelope_id: Some("ei".into()),
        payload: Some(serde_json::json!({
            "type": "block_actions", "trigger_id": "t9",
            "actions": [{ "action_id": "a1", "type": "button" }, { "action_id": "a2" }],
            "user": { "id": "U1" }, "channel": { "id": "C1", "name": "general" }
        })),
    };
    let p = InteractivePayload::from_envelope(&env).unwrap();
    assert_eq!(p.actions.len(), 2);
    assert_eq!(p.channel.as_ref().unwrap().id, "C1");
    let empty = SocketModeEnvelope {
        envelope_type: "interactive".into(),
        envelope_id: None,
        payload: None,
    };
    assert!(InteractivePayload::from_envelope(&empty).is_none());
}

#[test]
fn slash_command_parsing() {
    let env = SocketModeEnvelope {
        envelope_type: "slash_commands".into(),
        envelope_id: Some("es".into()),
        payload: Some(serde_json::json!({
            "command": "/status", "text": "all", "channel_id": "C2",
            "user_id": "U2", "response_url": "https://hooks.slack.com/xxx"
        })),
    };
    let cmd = SlashCommandPayload::from_envelope(&env).unwrap();
    assert_eq!(cmd.command, "/status");
    assert_eq!(
        cmd.response_url.as_deref(),
        Some("https://hooks.slack.com/xxx")
    );
}

// --- Block Kit builder ---

#[test]
fn block_kit_builder() {
    let msg = BlockKitMessage::new();
    assert!(msg.is_empty());
    assert_eq!(msg.to_json(), serde_json::json!([]));

    let msg = BlockKitMessage::new()
        .add_header("Welcome")
        .add_divider()
        .add_section(TextObject::mrkdwn("Info"))
        .add_actions(vec![BlockElement::Button {
            text: TextObject::plain("Click"),
            action_id: "b".into(),
            value: Some("go".into()),
            style: Some("primary".into()),
        }])
        .add_context(vec![ContextElement::Mrkdwn {
            text: "footer".into(),
        }]);

    let arr = msg.to_json();
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 5);
    assert_eq!(arr[0]["type"], "header");
    assert_eq!(arr[1]["type"], "divider");
    assert_eq!(arr[2]["type"], "section");
    assert_eq!(arr[3]["type"], "actions");
    assert_eq!(arr[4]["type"], "context");
}

#[test]
fn slack_image_url_blocks_with_caption() {
    let (blocks, fallback) =
        slack_image_url_blocks("https://example.com/hero.png", Some("Release snapshot"));
    assert_eq!(fallback, "Release snapshot");
    let arr = blocks.as_array().expect("blocks array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["type"], "section");
    assert_eq!(arr[1]["type"], "image");
    assert_eq!(arr[1]["image_url"], "https://example.com/hero.png");
    assert_eq!(arr[1]["alt_text"], "Release snapshot");
}

#[test]
fn slack_image_url_blocks_without_caption() {
    let (blocks, fallback) = slack_image_url_blocks("https://example.com/hero.png", Some("   "));
    assert_eq!(fallback, "https://example.com/hero.png");
    let arr = blocks.as_array().expect("blocks array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "image");
    assert_eq!(arr[0]["alt_text"], "image");
}

#[test]
fn reactions_toggle_enabled_defaults_and_env_overrides() {
    assert!(reactions_toggle_enabled(None, true));
    assert!(!reactions_toggle_enabled(None, false));
    assert!(reactions_toggle_enabled(Some("true"), false));
    assert!(!reactions_toggle_enabled(Some("0"), true));
    assert!(!reactions_toggle_enabled(Some("no"), true));
    assert!(reactions_toggle_enabled(Some("1"), false));
}

#[test]
fn block_variants_serialize() {
    let sec = Block::section_with_accessory(
        TextObject::mrkdwn("Pick"),
        BlockElement::Button {
            text: TextObject::plain("Go"),
            action_id: "g".into(),
            value: None,
            style: None,
        },
    );
    assert_eq!(
        serde_json::to_value(&sec).unwrap()["accessory"]["type"],
        "button"
    );

    let fld = Block::section_with_fields(
        TextObject::mrkdwn("S"),
        vec![TextObject::mrkdwn("A"), TextObject::mrkdwn("B")],
    );
    assert_eq!(
        serde_json::to_value(&fld).unwrap()["fields"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let sel = BlockElement::StaticSelect {
        placeholder: TextObject::plain("Choose"),
        action_id: "s".into(),
        options: vec![SelectOption {
            text: TextObject::plain("X"),
            value: "x".into(),
        }],
    };
    assert_eq!(serde_json::to_value(&sel).unwrap()["type"], "static_select");
}

#[test]
fn block_kit_round_trip() {
    let msg = BlockKitMessage::new()
        .add_header("T")
        .add_section(TextObject::plain("b"));
    assert_eq!(
        serde_json::from_value::<Vec<Block>>(msg.to_json())
            .unwrap()
            .len(),
        2
    );
}

// --- Home tab & modal views ---

#[test]
fn home_view() {
    let view = HomeView::new(vec![Block::header("Home"), Block::divider()]);
    let j = view.to_json();
    assert_eq!(j["type"], "home");
    assert_eq!(j["blocks"].as_array().unwrap().len(), 2);

    let msg = BlockKitMessage::new()
        .add_header("H")
        .add_section(TextObject::plain("S"));
    let view2 = HomeView::from_block_kit(&msg);
    assert_eq!(view2.to_json()["blocks"].as_array().unwrap().len(), 2);
}

#[test]
fn modal_view() {
    let m = ModalView::new("Title", vec![Block::section(TextObject::plain("Body"))]);
    let j = m.to_json();
    assert_eq!(j["type"], "modal");
    assert_eq!(j["title"]["text"], "Title");
    assert!(j.get("submit").is_none());

    let m2 = ModalView::new("Confirm", vec![])
        .with_submit("Yes")
        .with_close("No")
        .with_callback_id("cb");
    let j2 = m2.to_json();
    assert_eq!(j2["submit"]["text"], "Yes");
    assert_eq!(j2["close"]["text"], "No");
    assert_eq!(j2["callback_id"], "cb");
}

// --- Response types & misc ---

#[test]
fn slack_user_deserializes() {
    let u: SlackUser = serde_json::from_value(serde_json::json!({
        "id": "U1", "name": "alice", "real_name": "Alice", "is_bot": false, "is_admin": true,
        "profile": { "email": "a@ex.com" }
    }))
    .unwrap();
    assert!(u.is_admin);
    assert_eq!(u.profile.unwrap().email.as_deref(), Some("a@ex.com"));
}

#[test]
fn user_info_response_variants() {
    let ok: UserInfoResponse = serde_json::from_value(
        serde_json::json!({ "ok": true, "user": { "id": "U1", "is_bot": true } }),
    )
    .unwrap();
    assert!(ok.user.unwrap().is_bot);
    let err: UserInfoResponse =
        serde_json::from_value(serde_json::json!({ "ok": false, "error": "user_not_found" }))
            .unwrap();
    assert_eq!(err.error.as_deref(), Some("user_not_found"));
}

#[tokio::test]
async fn list_user_conversations_paginates_and_skips_invalid_channels() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users.conversations"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "channels": [
                {"id": "C001", "name": "first", "is_private": false},
                {"id": "", "name": "no-id"},
                {"id": "C002"}
            ],
            "response_metadata": {"next_cursor": "cur1"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users.conversations"))
        .and(query_param("cursor", "cur1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "channels": [
                {"id": "G123", "name": "secret-chat", "is_private": true}
            ],
            "response_metadata": {"next_cursor": ""}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let adapter = SlackAdapter::new(SlackConfig {
        token: "xoxb-test-token".into(),
        app_token: None,
        socket_mode: false,
        reactions: true,
        require_mention: false,
        bot_user_id: None,
        mention_patterns: Vec::new(),
        proxy: AdapterProxyConfig::default(),
    })
    .expect("adapter");

    let entries = adapter
        .list_user_conversations_from_base(&server.uri())
        .await
        .expect("conversations");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["C001", "G123"]
    );
    assert_eq!(entries[0].kind.as_deref(), Some("channel"));
    assert_eq!(entries[1].kind.as_deref(), Some("private"));
}

#[tokio::test]
async fn list_user_conversations_not_ok_returns_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users.conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "error": "missing_scope"
        })))
        .mount(&server)
        .await;

    let adapter = SlackAdapter::new(SlackConfig {
        token: "xoxb-test-token".into(),
        app_token: None,
        socket_mode: false,
        reactions: true,
        require_mention: false,
        bot_user_id: None,
        mention_patterns: Vec::new(),
        proxy: AdapterProxyConfig::default(),
    })
    .expect("adapter");

    let err = adapter
        .list_user_conversations_from_base(&server.uri())
        .await
        .expect_err("missing scope");
    assert!(err.to_string().contains("missing_scope"));
}

#[test]
fn permalink_response_deserializes() {
    let r: PermalinkResponse = serde_json::from_value(serde_json::json!({
        "ok": true, "permalink": "https://ws.slack.com/archives/C1/p1", "channel": "C1"
    }))
    .unwrap();
    assert!(r.permalink.unwrap().contains("archives"));
}

#[test]
fn context_elements_serialize() {
    let block = Block::context(vec![
        ContextElement::Mrkdwn {
            text: "by *bot*".into(),
        },
        ContextElement::PlainText { text: "now".into() },
        ContextElement::Image {
            image_url: "https://x.com/i.png".into(),
            alt_text: "i".into(),
        },
    ]);
    let elems = serde_json::to_value(&block).unwrap()["elements"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(elems.len(), 3);
    assert_eq!(elems[0]["type"], "mrkdwn");
}

#[test]
fn split_message_at_newline_boundary() {
    let text = format!("{}\n{}", "a".repeat(3999), "b".repeat(100));
    assert_eq!(split_message(&text, 4000).len(), 2);
}
