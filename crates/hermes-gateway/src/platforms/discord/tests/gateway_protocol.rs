use tokio::time::{sleep, Duration, Instant};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

#[tokio::test]
async fn discord_liveness_probe_uses_rest_users_me_and_bot_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .and(header("Authorization", "Bot test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "bot-self",
            "username": "Hermes"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    probe_discord_rest_liveness(&client, "test-token", &server.uri())
        .await
        .expect("liveness probe should succeed");
}

#[tokio::test]
async fn discord_liveness_failure_marks_adapter_offline_for_reconnect_watcher() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/@me"))
        .respond_with(ResponseTemplate::new(500).set_body_string("proxy wedged"))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.api_base_url = Some(server.uri());
    cfg.liveness_interval_seconds = 0.01;
    cfg.liveness_failure_threshold = 2;
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    adapter.start().await.expect("start");
    assert!(adapter.is_running());

    let deadline = Instant::now() + Duration::from_secs(2);
    while adapter.is_running() && Instant::now() < deadline {
        sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !adapter.is_running(),
        "liveness failures should mark adapter offline for platform_reconnect_watcher"
    );
    adapter.stop().await.expect("stop");
}

#[tokio::test]
async fn discord_liveness_probe_disabled_by_zero_knob() {
    let mut cfg = test_config();
    cfg.liveness_interval_seconds = 0.0;
    cfg.liveness_failure_threshold = 1;
    let adapter = DiscordAdapter::new(cfg).expect("adapter");
    adapter.start().await.expect("start");
    assert!(adapter.is_running());
    assert!(adapter
        .liveness_task
        .lock()
        .expect("liveness lock")
        .is_none());
    adapter.stop().await.expect("stop");
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
fn split_message_long_unicode_is_char_boundary_safe() {
    let text = "é".repeat(2001);
    let chunks = split_message(&text, 2000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chars().count(), 2000);
    assert_eq!(chunks[1], "é");
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
