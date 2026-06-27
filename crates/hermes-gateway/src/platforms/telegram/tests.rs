use super::*;
use serde_json::Value;
use std::sync::Mutex;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct JsonFieldAbsent(&'static str);

impl Match for JsonFieldAbsent {
    fn matches(&self, request: &Request) -> bool {
        request
            .body_json::<Value>()
            .ok()
            .and_then(|v| v.as_object().cloned())
            .map(|obj| !obj.contains_key(self.0))
            .unwrap_or(false)
    }
}

fn test_config() -> TelegramConfig {
    TelegramConfig {
        token: "fake_token_12345".into(),
        webhook_url: None,
        webhook_secret: None,
        polling: true,
        proxy: AdapterProxyConfig::default(),
        parse_markdown: false,
        parse_html: false,
        disable_link_previews: false,
        rich_messages: true,
        poll_timeout: 30,
        reply_to_mode: "first".into(),
        reactions: false,
        fallback_ips: Vec::new(),
        require_mention: false,
        guest_mode: false,
        free_response_chats: Vec::new(),
        allowed_chats: Vec::new(),
        group_allowed_chats: Vec::new(),
        ignored_threads: Vec::new(),
        allowed_topics: Vec::new(),
        mention_patterns: Vec::new(),
        exclusive_bot_mentions: false,
        observe_unmentioned_group_messages: false,
        text_batch_delay_ms: DEFAULT_TEXT_BATCH_DELAY_MS,
        bot_username: None,
        command_menu_enabled: true,
        command_menu_max_commands: DEFAULT_TELEGRAM_COMMAND_MENU_MAX,
        command_menu_priority: Vec::new(),
        command_menu_priority_mode: "prepend".into(),
    }
}

fn test_adapter(config: TelegramConfig) -> TelegramAdapter {
    TelegramAdapter::new(config).unwrap()
}

// -----------------------------------------------------------------------
// split_message tests (original)
// -----------------------------------------------------------------------

#[test]
fn split_message_short() {
    let chunks = split_message("hello", 4096);
    assert_eq!(chunks, vec!["hello"]);
}

#[test]
fn split_message_exact_boundary() {
    let text = "a".repeat(4096);
    let chunks = split_message(&text, 4096);
    assert_eq!(chunks.len(), 1);
}

#[test]
fn split_message_long() {
    let text = "a".repeat(5000);
    let chunks = split_message(&text, 4096);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 4096);
    assert_eq!(chunks[1].len(), 904);
}

#[tokio::test]
async fn edit_text_clips_overflow_preview_instead_of_erroring() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/editMessageText"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 456 }
        })))
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());
    let text = "a".repeat(MAX_MESSAGE_LENGTH + 1);

    adapter
        .edit_text("123", "456", &text, None)
        .await
        .expect("overflow edit clips to one preview message");

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().expect("json body");
    let clipped = body
        .get("text")
        .and_then(Value::as_str)
        .expect("clipped text");
    assert_eq!(clipped.chars().count(), MAX_MESSAGE_LENGTH);
    assert!(clipped.ends_with("..."));
}

#[test]
fn split_message_prefers_newline() {
    let mut text = "a".repeat(4000);
    text.push('\n');
    text.push_str(&"b".repeat(200));
    let chunks = split_message(&text, 4096);
    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].ends_with('\n'));
}

#[test]
fn telegram_rich_normalizes_prose_linebreaks_only() {
    let normalized = TelegramAdapter::rich_normalize_linebreaks(
            "intro\nnext\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\nend",
        );

    assert!(normalized.starts_with("intro  \nnext\n\n"));
    assert!(normalized.contains("```rust\nlet x = 1;\nlet y = 2;\n```"));
    assert!(normalized.contains("| A | B |\n|---|---|\n| 1 | 2 |"));
}

#[test]
fn telegram_rich_cjk_guard_matches_desktop_garble_scripts() {
    assert!(TelegramAdapter::has_telegram_desktop_cjk_rich_garble_shape(
        "table 你好"
    ));
    assert!(TelegramAdapter::has_telegram_desktop_cjk_rich_garble_shape(
        "かな"
    ));
    assert!(TelegramAdapter::has_telegram_desktop_cjk_rich_garble_shape(
        "한글"
    ));
    assert!(!TelegramAdapter::has_telegram_desktop_cjk_rich_garble_shape("plain ascii table"));
}

#[test]
fn telegram_command_menu_prioritizes_and_caps_commands() {
    let mut cfg = test_config();
    cfg.command_menu_max_commands = 3;
    cfg.command_menu_priority = vec!["status".into(), "/model".into()];
    let adapter = test_adapter(cfg);

    let commands = adapter.command_menu_commands();
    let names = commands
        .iter()
        .map(|command| command.command.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["status", "model", "start"]);
}

#[tokio::test]
async fn telegram_start_registers_command_menu_for_core_scopes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/setMyCommands"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": true
        })))
        .expect(3)
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.command_menu_max_commands = 2;
    cfg.command_menu_priority = vec!["status".into()];
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    adapter.start().await.expect("start");

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 3);
    let scopes = requests
        .iter()
        .map(|request| {
            let body: Value = request.body_json().expect("json body");
            assert_eq!(
                body.pointer("/commands/0/command").and_then(Value::as_str),
                Some("status")
            );
            assert_eq!(
                body.pointer("/commands/1/command").and_then(Value::as_str),
                Some("start")
            );
            body.pointer("/scope/type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(scopes.contains(&"default".to_string()));
    assert!(scopes.contains(&"all_private_chats".to_string()));
    assert!(scopes.contains(&"all_group_chats".to_string()));
}

#[test]
fn telegram_reply_to_mode_parse_and_chunk_policy() {
    assert_eq!(TelegramReplyToMode::parse(None), TelegramReplyToMode::First);
    assert_eq!(
        TelegramReplyToMode::parse(Some("off")),
        TelegramReplyToMode::Off
    );
    assert_eq!(
        TelegramReplyToMode::parse(Some("ALL")),
        TelegramReplyToMode::All
    );
    assert_eq!(
        TelegramReplyToMode::parse(Some("invalid")),
        TelegramReplyToMode::First
    );

    assert!(!TelegramReplyToMode::Off.references_chunk(0));
    assert!(TelegramReplyToMode::First.references_chunk(0));
    assert!(!TelegramReplyToMode::First.references_chunk(1));
    assert!(TelegramReplyToMode::All.references_chunk(10));
}

#[test]
fn telegram_should_thread_reply_respects_reply_mode() {
    let mut cfg = test_config();
    cfg.reply_to_mode = "off".into();
    let adapter = test_adapter(cfg);
    assert!(!adapter.should_thread_reply(Some(99), 0));

    let mut cfg = test_config();
    cfg.reply_to_mode = "first".into();
    let adapter = test_adapter(cfg);
    assert!(adapter.should_thread_reply(Some(99), 0));
    assert!(!adapter.should_thread_reply(Some(99), 1));
    assert!(!adapter.should_thread_reply(None, 0));

    let mut cfg = test_config();
    cfg.reply_to_mode = "all".into();
    let adapter = test_adapter(cfg);
    assert!(adapter.should_thread_reply(Some(99), 0));
    assert!(adapter.should_thread_reply(Some(99), 2));
}

#[test]
fn telegram_split_gateway_chat_thread_preserves_topic_suffix() {
    assert_eq!(
        TelegramAdapter::split_gateway_chat_thread("-1001:17585"),
        ("-1001", Some(17585))
    );
    assert_eq!(
        TelegramAdapter::split_gateway_chat_thread("-1001:0"),
        ("-1001:0", None)
    );
    assert_eq!(
        TelegramAdapter::split_gateway_chat_thread("room:server"),
        ("room:server", None)
    );
}

#[test]
fn telegram_merge_caption_uses_exact_dedupe() {
    assert_eq!(TelegramAdapter::merge_caption(None, "Hello"), "Hello");
    assert_eq!(
        TelegramAdapter::merge_caption(Some("Revenue"), "Revenue  "),
        "Revenue"
    );
    assert_eq!(
        TelegramAdapter::merge_caption(Some("Meeting agenda"), "Meeting"),
        "Meeting agenda\n\nMeeting"
    );
    assert_eq!(
        TelegramAdapter::merge_caption(Some("Revenue"), "Revenue and Profit"),
        "Revenue\n\nRevenue and Profit"
    );
    let merged = TelegramAdapter::merge_caption(Some("A\n\nB"), "A");
    assert_eq!(merged, "A\n\nB");
}

#[test]
fn telegram_webhook_secret_required_only_for_webhook_mode() {
    let polling = test_config();
    assert!(TelegramAdapter::new(polling).is_ok());

    let mut webhook = test_config();
    webhook.webhook_url = Some("https://hooks.example.com/tg".into());
    let err = match TelegramAdapter::new(webhook) {
        Ok(_) => panic!("webhook mode without secret must fail"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("TELEGRAM_WEBHOOK_SECRET"));
    assert!(err.contains("GHSA-3vpc-7q5r-276h"));
    assert!(err.contains("openssl rand"));

    let mut webhook = test_config();
    webhook.webhook_url = Some("https://hooks.example.com/tg".into());
    webhook.webhook_secret = Some("secret-token".into());
    assert!(TelegramAdapter::new(webhook).is_ok());
}

#[tokio::test]
async fn telegram_reactions_call_set_message_reaction_when_enabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/setMessageReaction"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": true
        })))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.reactions = true;
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    adapter.add_reaction("123", "456", "👀").await.unwrap();
    adapter.remove_reaction("123", "456", "👀").await.unwrap();

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2);
    let add_json: Value = serde_json::from_slice(&requests[0].body).expect("add json");
    assert_eq!(
        add_json.pointer("/chat_id").and_then(|v| v.as_str()),
        Some("123")
    );
    assert_eq!(
        add_json.pointer("/message_id").and_then(|v| v.as_i64()),
        Some(456)
    );
    assert_eq!(
        add_json
            .pointer("/reaction/0/emoji")
            .and_then(|v| v.as_str()),
        Some("👀")
    );
    let remove_json: Value = serde_json::from_slice(&requests[1].body).expect("remove json");
    assert_eq!(
        remove_json
            .pointer("/reaction")
            .and_then(|v| v.as_array())
            .map(Vec::len),
        Some(0)
    );
}

#[tokio::test]
async fn telegram_reactions_are_noop_when_disabled() {
    let server = MockServer::start().await;
    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    adapter.add_reaction("123", "456", "👀").await.unwrap();

    let requests = server.received_requests().await.expect("requests");
    assert!(requests.is_empty());
}

#[test]
fn telegram_network_fallback_ips_filter_and_deduplicate() {
    let raw = vec![
        "149.154.167.220".to_string(),
        "not-valid".to_string(),
        "149.154.167.220,149.154.167.221".to_string(),
        "::1".to_string(),
    ];
    let addrs = TelegramAdapter::fallback_socket_addrs(&raw);
    let rendered = addrs
        .iter()
        .map(|addr| addr.ip().to_string())
        .collect::<Vec<_>>();
    assert_eq!(rendered, vec!["149.154.167.220", "149.154.167.221", "::1"]);

    let mut cfg = test_config();
    cfg.fallback_ips = raw;
    assert!(TelegramAdapter::new(cfg).is_ok());
}

#[test]
fn telegram_group_gating_covers_mentions_guests_threads_and_topics() {
    let mut cfg = test_config();
    cfg.bot_username = Some("hermes_bot".into());
    cfg.require_mention = true;
    cfg.allowed_chats = vec!["-100".into()];
    cfg.guest_mode = true;
    cfg.mention_patterns = vec![r"^\s*chompy\b".into(), "(".into()];
    cfg.ignored_threads = vec!["31".into()];
    cfg.allowed_topics = vec!["8".into(), "0".into()];
    let adapter = test_adapter(cfg);

    let mut msg = make_text_message(
        1,
        make_chat(-100, "supergroup"),
        make_user(11, Some("u")),
        "hello",
    );
    msg.message_thread_id = Some(8);
    assert!(!adapter.should_process_message(&msg, false));

    msg.text = Some("hi @hermes_bot".into());
    msg.entities = vec![MessageEntity {
        entity_type: "mention".into(),
        offset: 3,
        length: 11,
    }];
    assert!(adapter.should_process_message(&msg, false));

    msg.text = Some("chompy status".into());
    msg.entities.clear();
    assert!(adapter.should_process_message(&msg, false));

    msg.message_thread_id = Some(31);
    assert!(!adapter.should_process_message(&msg, false));

    msg.message_thread_id = Some(9);
    msg.text = Some("hi @hermes_bot".into());
    msg.entities = vec![MessageEntity {
        entity_type: "mention".into(),
        offset: 3,
        length: 11,
    }];
    assert!(!adapter.should_process_message(&msg, false));

    msg.chat.id = -200;
    msg.message_thread_id = Some(8);
    assert!(adapter.should_process_message(&msg, false));

    msg.text = Some("chompy status".into());
    msg.entities.clear();
    assert!(!adapter.should_process_message(&msg, false));
}

#[test]
fn telegram_text_batcher_aggregates_by_chat_user_and_thread() {
    let now = Instant::now();
    let mut batcher = TelegramTextBatcher::new(Duration::from_millis(50));
    let mut first = IncomingMessage {
        chat_id: 1,
        user_id: Some(2),
        username: None,
        text: Some("part one".into()),
        message_id: 10,
        is_voice: false,
        is_photo: false,
        is_sticker: false,
        is_document: false,
        voice_file_id: None,
        photo_file_id: None,
        sticker_file_id: None,
        document_file_id: None,
        document_file_name: None,
        document_mime_type: None,
        document_file_size: None,
        reply_to_message_id: None,
        message_thread_id: Some(8),
        chat_type: ChatKind::Private,
        is_group: false,
        callback_query_id: None,
        callback_data: None,
    };
    batcher.enqueue_at(first.clone(), now);
    first.text = Some("part two".into());
    first.message_id = 11;
    batcher.enqueue_at(first, now + Duration::from_millis(10));
    assert!(batcher
        .drain_ready_at(now + Duration::from_millis(40))
        .is_empty());
    let ready = batcher.drain_ready_at(now + Duration::from_millis(70));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].text.as_deref(), Some("part one\npart two"));
    assert_eq!(batcher.pending_len(), 0);
}

#[test]
fn telegram_topic_binding_store_recovers_only_lobby_replies() {
    let mut store = TelegramTopicBindingStore::default();
    store.enable("208214988", "user1");
    store.bind("208214988", "111", "session-a", "user1", None, false);
    store.bind("208214988", "222", "session-b", "user1", None, false);

    assert_eq!(
        store.recover_thread_id("208214988", "user1", None),
        Some("222".into())
    );
    assert_eq!(
        store.recover_thread_id("208214988", "user1", Some("0")),
        Some("222".into())
    );
    assert_eq!(
        store.recover_thread_id("208214988", "user1", Some("9999")),
        None
    );
    assert_eq!(store.get_by_session("session-b").unwrap().thread_id, "222");
    assert_eq!(
        store
            .list_for_chat("208214988")
            .into_iter()
            .map(|binding| binding.thread_id)
            .collect::<Vec<_>>(),
        vec!["222", "111"]
    );
    store.remove_session("session-b");
    assert!(store.get("208214988", "222").is_none());
    assert!(store.remove("208214988", "111"));
    assert!(store.get("208214988", "111").is_none());
    assert!(!store.remove("208214988", "missing"));
    store.disable("208214988", "user1");
    assert!(!store.is_enabled("208214988", "user1"));
}

#[tokio::test]
async fn telegram_thread_fallback_retries_without_thread_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendMessage"))
        .and(body_partial_json(serde_json::json!({
            "message_thread_id": 999
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "description": "Bad Request: message thread not found"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendMessage"))
        .and(JsonFieldAbsent("message_thread_id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 77 }
        })))
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());
    adapter.bind_dm_topic("123", "999", "session-dead-topic", "user1", None, false);
    assert!(adapter.dm_topic_binding("123", "999").is_some());

    let ids = adapter
        .send_text_with_keyboard(
            "123",
            "hello [world]_1",
            InlineKeyboardMarkup {
                inline_keyboard: vec![vec![InlineKeyboardButton {
                    text: "Go".into(),
                    callback_data: Some("go".into()),
                    url: None,
                }]],
            },
            Some("MarkdownV2"),
            Some(55),
            Some(999),
        )
        .await
        .unwrap();
    assert_eq!(ids, vec![77]);
    assert!(adapter.dm_topic_binding("123", "999").is_none());
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2);
    let first: Value = requests[0].body_json().expect("json");
    assert_eq!(
        first.pointer("/message_thread_id").and_then(|v| v.as_i64()),
        Some(999)
    );
    let second: Value = requests[1].body_json().expect("json");
    assert!(second.get("message_thread_id").is_none());
    assert!(second.get("reply_to_message_id").is_none());
    assert_eq!(
        second
            .pointer("/reply_markup/inline_keyboard/0/0/callback_data")
            .and_then(|v| v.as_str()),
        Some("go")
    );
    assert_eq!(
        second.pointer("/parse_mode").and_then(|v| v.as_str()),
        Some("MarkdownV2")
    );
    assert!(second
        .pointer("/text")
        .and_then(|v| v.as_str())
        .unwrap()
        .contains("\\[world\\]\\_1"));
}

#[tokio::test]
async fn telegram_rich_message_uses_bot_api_10_1_payload_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendRichMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "-1001",
            "message_thread_id": 42,
            "rich_message": { "markdown": "| A | B |\n|---|---|\n| 1 | 2 |" },
            "reply_parameters": { "message_id": 55 },
            "link_preview_options": { "is_disabled": true }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 91 }
        })))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.rich_messages = true;
    cfg.disable_link_previews = true;
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text(
            "-1001:42",
            "| A | B |\n|---|---|\n| 1 | 2 |",
            Some("MarkdownV2"),
            Some(55),
        )
        .await
        .unwrap();
    assert_eq!(ids, vec![91]);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().expect("json body");
    assert!(body.get("text").is_none());
    assert!(body.get("parse_mode").is_none());
    assert!(body.get("reply_to_message_id").is_none());
    assert_eq!(
        body.pointer("/rich_message/markdown")
            .and_then(|v| v.as_str()),
        Some("| A | B |\n|---|---|\n| 1 | 2 |")
    );
}

#[tokio::test]
async fn telegram_rich_messages_stay_legacy_for_non_eligible_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "text": "plain **markdown**"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 93 }
        })))
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("123", "plain **markdown**", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![93]);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.path().ends_with("/sendMessage"));
}

#[tokio::test]
async fn telegram_pipe_tables_auto_use_rich_messages_when_default_off() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendRichMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "rich_message": { "markdown": "| A | B |\n|---|---|\n| 1 | 2 |" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 94 }
        })))
        .mount(&server)
        .await;

    let cfg: TelegramConfig =
        serde_json::from_str(r#"{"token":"fake_token_12345"}"#).expect("config");
    assert!(!cfg.rich_messages);
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("123", "| A | B |\n|---|---|\n| 1 | 2 |", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![94]);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.path().ends_with("/sendRichMessage"));
}

#[tokio::test]
async fn telegram_pipe_table_auto_rich_routes_encoded_topic_without_reply_anchor() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendRichMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "-1001",
            "message_thread_id": 42,
            "rich_message": { "markdown": "| A | B |\n|---|---|\n| 1 | 2 |" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 95 }
        })))
        .mount(&server)
        .await;

    let cfg: TelegramConfig =
        serde_json::from_str(r#"{"token":"fake_token_12345"}"#).expect("config");
    assert!(!cfg.rich_messages);
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("-1001:42", "| A | B |\n|---|---|\n| 1 | 2 |", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![95]);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().expect("json body");
    assert!(body.get("reply_parameters").is_none());
    assert_eq!(
        body.pointer("/chat_id").and_then(|v| v.as_str()),
        Some("-1001")
    );
    assert_eq!(
        body.pointer("/message_thread_id").and_then(|v| v.as_i64()),
        Some(42)
    );
}

#[tokio::test]
async fn telegram_non_table_rich_constructs_stay_legacy_when_default_off() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "text": "- [ ] one\n- [x] two"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 95 }
        })))
        .mount(&server)
        .await;

    let cfg: TelegramConfig =
        serde_json::from_str(r#"{"token":"fake_token_12345"}"#).expect("config");
    assert!(!cfg.rich_messages);
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("123", "- [ ] one\n- [x] two", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![95]);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.path().ends_with("/sendMessage"));
}

#[test]
fn telegram_pipe_table_primary_excludes_other_rich_constructs() {
    assert!(TelegramAdapter::content_is_pipe_table_primary(
        "| A | B |\n|---|---|\n| 1 | 2 |"
    ));
    assert!(!TelegramAdapter::content_is_pipe_table_primary(
        "| A | B |\n|---|---|\n- [ ] task"
    ));
    assert!(!TelegramAdapter::content_is_pipe_table_primary(
        "| A | B |\n|---|---|\n<details>extra</details>"
    ));
    assert!(!TelegramAdapter::content_is_pipe_table_primary(
        "| A | B |\n|---|---|\n$$x$$"
    ));
}

#[tokio::test]
async fn telegram_rich_message_hard_breaks_single_newlines() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendRichMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "rich_message": { "markdown": "- [ ] one  \n- [x] two" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 95 }
        })))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.rich_messages = true;
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("123", "- [ ] one\n- [x] two", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![95]);
}

#[tokio::test]
async fn telegram_rich_message_skips_cjk_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "text": "| A | B |\n|---|---|\n| 你好 | 2 |"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 96 }
        })))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.rich_messages = true;
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("123", "| A | B |\n|---|---|\n| 你好 | 2 |", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![96]);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.path().ends_with("/sendMessage"));
}

#[tokio::test]
async fn telegram_rich_message_bad_request_falls_back_to_legacy_send() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendRichMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "description": "Bad Request: rich message is invalid"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "text": "| A | B |\n|---|---|\n| 1 | 2 |",
            "disable_web_page_preview": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 92 }
        })))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.rich_messages = true;
    cfg.disable_link_previews = true;
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("123", "| A | B |\n|---|---|\n| 1 | 2 |", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![92]);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.path().ends_with("/sendRichMessage"));
    assert!(requests[1].url.path().ends_with("/sendMessage"));
}

#[test]
fn telegram_rich_skips_desktop_details_math_crash_shape() {
    assert!(
        TelegramAdapter::has_telegram_desktop_details_math_crash_shape(
            "<details><summary>Math</summary>$$x^2$$</details>"
        )
    );
    assert!(
        !TelegramAdapter::has_telegram_desktop_details_math_crash_shape(
            "<details><summary>Plain</summary>No math here</details>"
        )
    );
}

#[tokio::test]
async fn telegram_rich_edit_uses_bot_api_10_1_payload_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/editMessageText"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "message_id": 555,
            "rich_message": { "markdown": "| A | B |\n|---|---|\n| 1 | 2 |" },
            "link_preview_options": { "is_disabled": true }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 555 }
        })))
        .mount(&server)
        .await;

    let mut cfg = test_config();
    cfg.disable_link_previews = true;
    let mut adapter = test_adapter(cfg);
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    adapter
        .edit_text(
            "123",
            "555",
            "| A | B |\n|---|---|\n| 1 | 2 |",
            Some("MarkdownV2"),
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().expect("json body");
    assert!(body.get("text").is_none());
    assert!(body.get("parse_mode").is_none());
    assert_eq!(
        body.pointer("/rich_message/markdown")
            .and_then(|v| v.as_str()),
        Some("| A | B |\n|---|---|\n| 1 | 2 |")
    );
}

#[tokio::test]
async fn telegram_rich_edit_routes_encoded_topic_thread() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/editMessageText"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "-1001",
            "message_id": 555,
            "message_thread_id": 42,
            "rich_message": { "markdown": "| A | B |\n|---|---|\n| 1 | 2 |" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 555 }
        })))
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    adapter
        .edit_text("-1001:42", "555", "| A | B |\n|---|---|\n| 1 | 2 |", None)
        .await
        .unwrap();

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().expect("json body");
    assert_eq!(
        body.pointer("/chat_id").and_then(|v| v.as_str()),
        Some("-1001")
    );
    assert_eq!(
        body.pointer("/message_thread_id").and_then(|v| v.as_i64()),
        Some(42)
    );
    assert!(body.get("text").is_none());
    assert!(body.get("parse_mode").is_none());
}

#[tokio::test]
async fn telegram_rich_edit_bad_request_falls_back_to_legacy_edit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/editMessageText"))
        .and(body_partial_json(serde_json::json!({
            "rich_message": { "markdown": "| A | B |\n|---|---|\n| 1 | 2 |" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "description": "Bad Request: rich message is invalid"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/editMessageText"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "123",
            "message_id": 555,
            "text": "| A | B |\n|---|---|\n| 1 | 2 |"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 555 }
        })))
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    adapter
        .edit_text("123", "555", "| A | B |\n|---|---|\n| 1 | 2 |", None)
        .await
        .unwrap();

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2);
    let first: Value = requests[0].body_json().expect("first json");
    assert!(first.get("rich_message").is_some());
    let second: Value = requests[1].body_json().expect("second json");
    assert!(second.get("text").is_some());
}

#[tokio::test]
async fn telegram_encoded_gateway_chat_id_sends_to_topic_thread() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/sendMessage"))
        .and(body_partial_json(serde_json::json!({
            "chat_id": "-1001",
            "message_thread_id": 17585,
            "text": "topic hello"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 88 }
        })))
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let ids = adapter
        .send_text("-1001:17585", "topic hello", None, None)
        .await
        .unwrap();
    assert_eq!(ids, vec![88]);
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().expect("json body");
    assert_eq!(
        body.pointer("/chat_id").and_then(|v| v.as_str()),
        Some("-1001")
    );
    assert_eq!(
        body.pointer("/message_thread_id").and_then(|v| v.as_i64()),
        Some(17585)
    );
}

#[test]
fn telegram_media_method_matches_document_contracts() {
    assert_eq!(
        TelegramAdapter::media_method_for_extension("mp3"),
        ("sendAudio", "audio")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("wav"),
        ("sendDocument", "document")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("flac"),
        ("sendDocument", "document")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("mp4"),
        ("sendVideo", "video")
    );
}

#[test]
fn telegram_approval_callback_parses_and_tracks_state() {
    assert_eq!(
        parse_approval_callback("approval:once:42"),
        Some((ApprovalChoice::Once, 42))
    );
    assert_eq!(
        parse_approval_callback("approval:session:42"),
        Some((ApprovalChoice::Session, 42))
    );
    assert_eq!(
        parse_approval_callback("approval:deny:42"),
        Some((ApprovalChoice::Deny, 42))
    );
    assert_eq!(parse_approval_callback("model:pick:gpt"), None);
    assert_eq!(truncate_chars("abcdef", 5), "ab...");
}

// -----------------------------------------------------------------------
// parse_update tests (original, updated for new fields)
// -----------------------------------------------------------------------

fn make_chat(id: i64, chat_type: &str) -> Chat {
    Chat {
        id,
        chat_type: chat_type.into(),
        title: None,
        username: None,
    }
}

fn make_user(id: i64, username: Option<&str>) -> User {
    User {
        id,
        first_name: Some("Test".into()),
        username: username.map(|s| s.to_string()),
        is_bot: Some(false),
    }
}

fn make_text_message(msg_id: i64, chat: Chat, user: User, text: &str) -> TelegramMessage {
    TelegramMessage {
        message_id: msg_id,
        chat,
        from: Some(user),
        text: Some(text.into()),
        voice: None,
        photo: None,
        caption: None,
        entities: Vec::new(),
        caption_entities: Vec::new(),
        sticker: None,
        document: None,
        reply_to_message: None,
        rich_message: None,
        message_thread_id: None,
        is_topic_message: None,
    }
}

#[test]
fn parse_update_text_message() {
    let update = Update {
        update_id: 1,
        message: Some(make_text_message(
            42,
            make_chat(100, "private"),
            make_user(200, Some("testuser")),
            "hello bot",
        )),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert_eq!(incoming.chat_id, 100);
    assert_eq!(incoming.user_id, Some(200));
    assert_eq!(incoming.text, Some("hello bot".into()));
    assert!(!incoming.is_voice);
    assert!(!incoming.is_photo);
    assert!(!incoming.is_sticker);
    assert!(!incoming.is_document);
    assert!(!incoming.is_group);
    assert_eq!(incoming.chat_type, ChatKind::Private);
    assert!(incoming.callback_query_id.is_none());
}

#[test]
fn parse_update_voice_message() {
    let update = Update {
        update_id: 2,
        message: Some(TelegramMessage {
            message_id: 43,
            chat: make_chat(100, "private"),
            from: Some(make_user(200, None)),
            text: None,
            voice: Some(Voice {
                file_id: "voice123".into(),
                file_unique_id: "unique123".into(),
                duration: 5,
            }),
            photo: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: None,
            rich_message: None,
            message_thread_id: None,
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert!(incoming.is_voice);
    assert_eq!(incoming.voice_file_id, Some("voice123".into()));
}

#[test]
fn parse_update_photo_message() {
    let update = Update {
        update_id: 3,
        message: Some(TelegramMessage {
            message_id: 44,
            chat: make_chat(100, "group"),
            from: Some(make_user(200, None)),
            text: None,
            voice: None,
            photo: Some(vec![
                PhotoSize {
                    file_id: "small".into(),
                    file_unique_id: "s1".into(),
                    width: 90,
                    height: 90,
                },
                PhotoSize {
                    file_id: "large".into(),
                    file_unique_id: "s2".into(),
                    width: 800,
                    height: 600,
                },
            ]),
            caption: Some("my photo".into()),
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: None,
            rich_message: None,
            message_thread_id: None,
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert!(incoming.is_photo);
    assert_eq!(incoming.photo_file_id, Some("large".into()));
    assert_eq!(incoming.text, Some("my photo".into()));
    assert!(incoming.is_group);
    assert_eq!(incoming.chat_type, ChatKind::Group);
}

#[test]
fn parse_update_no_message() {
    let update = Update {
        update_id: 4,
        message: None,
        callback_query: None,
    };
    assert!(TelegramAdapter::parse_update(&update).is_none());
}

// -----------------------------------------------------------------------
// Sticker tests
// -----------------------------------------------------------------------

#[test]
fn parse_update_sticker_message() {
    let update = Update {
        update_id: 10,
        message: Some(TelegramMessage {
            message_id: 100,
            chat: make_chat(300, "private"),
            from: Some(make_user(400, Some("stickeruser"))),
            text: None,
            voice: None,
            photo: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: Some(Sticker {
                file_id: "sticker_abc".into(),
                file_unique_id: "su_abc".into(),
                width: Some(512),
                height: Some(512),
                is_animated: Some(false),
                is_video: Some(false),
                emoji: Some("😀".into()),
                set_name: Some("TestPack".into()),
            }),
            document: None,
            reply_to_message: None,
            rich_message: None,
            message_thread_id: None,
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert!(incoming.is_sticker);
    assert_eq!(incoming.sticker_file_id, Some("sticker_abc".into()));
    assert!(!incoming.is_voice);
    assert!(!incoming.is_photo);
    assert!(!incoming.is_document);
}

#[test]
fn parse_update_document_message() {
    let update = Update {
        update_id: 11,
        message: Some(TelegramMessage {
            message_id: 101,
            chat: make_chat(301, "private"),
            from: Some(make_user(401, Some("docuser"))),
            text: None,
            voice: None,
            photo: None,
            caption: Some("document caption".into()),
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: Some(Document {
                file_id: "doc_abc".into(),
                file_unique_id: Some("du_abc".into()),
                file_name: Some("notes.md".into()),
                mime_type: Some("text/markdown".into()),
                file_size: Some(2048),
            }),
            reply_to_message: None,
            rich_message: None,
            message_thread_id: None,
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert!(incoming.is_document);
    assert_eq!(incoming.document_file_id, Some("doc_abc".into()));
    assert_eq!(incoming.document_file_name, Some("notes.md".into()));
    assert_eq!(incoming.document_mime_type, Some("text/markdown".into()));
    assert_eq!(incoming.document_file_size, Some(2048));
    assert_eq!(incoming.text, Some("document caption".into()));
}

// -----------------------------------------------------------------------
// Callback query tests
// -----------------------------------------------------------------------

#[test]
fn parse_update_callback_query() {
    let update = Update {
        update_id: 20,
        message: None,
        callback_query: Some(CallbackQuery {
            id: "cq_123".into(),
            from: make_user(500, Some("cbuser")),
            message: Some(TelegramMessage {
                message_id: 200,
                chat: make_chat(600, "private"),
                from: None,
                text: Some("Original message".into()),
                voice: None,
                photo: None,
                caption: None,
                entities: Vec::new(),
                caption_entities: Vec::new(),
                sticker: None,
                document: None,
                reply_to_message: None,
                rich_message: None,
                message_thread_id: None,
                is_topic_message: None,
            }),
            data: Some("btn_action_1".into()),
            chat_instance: Some("inst".into()),
        }),
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert_eq!(incoming.callback_query_id, Some("cq_123".into()));
    assert_eq!(incoming.callback_data, Some("btn_action_1".into()));
    assert_eq!(incoming.user_id, Some(500));
    assert_eq!(incoming.chat_id, 600);
    assert_eq!(incoming.message_id, 200);
    assert_eq!(incoming.text, Some("btn_action_1".into()));
}

#[test]
fn parse_update_callback_query_no_message() {
    let update = Update {
        update_id: 21,
        message: None,
        callback_query: Some(CallbackQuery {
            id: "cq_456".into(),
            from: make_user(500, None),
            message: None,
            data: Some("data".into()),
            chat_instance: None,
        }),
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert_eq!(incoming.callback_query_id, Some("cq_456".into()));
    assert_eq!(incoming.chat_id, 0);
    assert_eq!(incoming.message_id, 0);
}

// -----------------------------------------------------------------------
// Reply-to and thread_id tests
// -----------------------------------------------------------------------

#[test]
fn parse_update_with_reply_and_thread() {
    let reply_msg = TelegramMessage {
        message_id: 10,
        chat: make_chat(100, "supergroup"),
        from: Some(make_user(50, None)),
        text: Some("original".into()),
        voice: None,
        photo: None,
        caption: None,
        entities: Vec::new(),
        caption_entities: Vec::new(),
        sticker: None,
        document: None,
        reply_to_message: None,
        rich_message: None,
        message_thread_id: None,
        is_topic_message: None,
    };

    let update = Update {
        update_id: 30,
        message: Some(TelegramMessage {
            message_id: 55,
            chat: make_chat(100, "supergroup"),
            from: Some(make_user(200, None)),
            text: Some("replying".into()),
            voice: None,
            photo: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: Some(Box::new(reply_msg)),
            rich_message: None,
            message_thread_id: Some(999),
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert_eq!(incoming.reply_to_message_id, Some(10));
    assert_eq!(incoming.message_thread_id, Some(999));
    assert!(incoming.is_group);
    assert_eq!(incoming.chat_type, ChatKind::Supergroup);
}

#[test]
fn parse_update_text_reply_to_photo_exposes_replied_media() {
    let reply_msg = TelegramMessage {
        message_id: 51,
        chat: make_chat(100, "supergroup"),
        from: Some(make_user(50, None)),
        text: None,
        voice: None,
        photo: Some(vec![PhotoSize {
            file_id: "replied-large".into(),
            file_unique_id: "rp1".into(),
            width: 1280,
            height: 720,
        }]),
        caption: None,
        entities: Vec::new(),
        caption_entities: Vec::new(),
        sticker: None,
        document: None,
        reply_to_message: None,
        rich_message: None,
        message_thread_id: None,
        is_topic_message: None,
    };

    let update = Update {
        update_id: 31,
        message: Some(TelegramMessage {
            message_id: 56,
            chat: make_chat(100, "supergroup"),
            from: Some(make_user(200, None)),
            text: Some("what is in this image?".into()),
            voice: None,
            photo: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: Some(Box::new(reply_msg)),
            rich_message: None,
            message_thread_id: Some(999),
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert_eq!(incoming.reply_to_message_id, Some(51));
    assert_eq!(incoming.text, Some("what is in this image?".into()));
    assert!(incoming.is_photo);
    assert_eq!(incoming.photo_file_id, Some("replied-large".into()));
}

#[test]
fn parse_update_current_media_does_not_merge_replied_media() {
    let reply_msg = TelegramMessage {
        message_id: 53,
        chat: make_chat(100, "supergroup"),
        from: Some(make_user(50, None)),
        text: None,
        voice: Some(Voice {
            file_id: "replied-voice".into(),
            file_unique_id: "voice-unique".into(),
            duration: 7,
        }),
        photo: None,
        caption: None,
        entities: Vec::new(),
        caption_entities: Vec::new(),
        sticker: None,
        document: None,
        reply_to_message: None,
        rich_message: None,
        message_thread_id: None,
        is_topic_message: None,
    };

    let update = Update {
        update_id: 33,
        message: Some(TelegramMessage {
            message_id: 58,
            chat: make_chat(100, "supergroup"),
            from: Some(make_user(200, None)),
            text: None,
            voice: None,
            photo: Some(vec![PhotoSize {
                file_id: "current-photo".into(),
                file_unique_id: "current-photo-unique".into(),
                width: 640,
                height: 480,
            }]),
            caption: Some("caption".into()),
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: Some(Box::new(reply_msg)),
            rich_message: None,
            message_thread_id: None,
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert!(incoming.is_photo);
    assert_eq!(incoming.photo_file_id, Some("current-photo".into()));
    assert!(!incoming.is_voice);
    assert_eq!(incoming.voice_file_id, None);
}

#[test]
fn parse_update_text_reply_ignores_oversized_replied_document() {
    let reply_msg = TelegramMessage {
        message_id: 52,
        chat: make_chat(100, "supergroup"),
        from: Some(make_user(50, None)),
        text: None,
        voice: None,
        photo: None,
        caption: None,
        entities: Vec::new(),
        caption_entities: Vec::new(),
        sticker: None,
        document: Some(Document {
            file_id: "huge-doc".into(),
            file_unique_id: Some("huge-unique".into()),
            file_name: Some("huge.pdf".into()),
            mime_type: Some("application/pdf".into()),
            file_size: Some(TELEGRAM_MAX_DOCUMENT_SIZE_BYTES + 1),
        }),
        reply_to_message: None,
        rich_message: None,
        message_thread_id: None,
        is_topic_message: None,
    };

    let update = Update {
        update_id: 32,
        message: Some(TelegramMessage {
            message_id: 57,
            chat: make_chat(100, "supergroup"),
            from: Some(make_user(200, None)),
            text: Some("read this".into()),
            voice: None,
            photo: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: Some(Box::new(reply_msg)),
            rich_message: None,
            message_thread_id: None,
            is_topic_message: None,
        }),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert_eq!(incoming.reply_to_message_id, Some(52));
    assert!(!incoming.is_document);
    assert_eq!(incoming.document_file_id, None);
}

// -----------------------------------------------------------------------
// Group chat / ChatKind tests
// -----------------------------------------------------------------------

#[test]
fn chat_kind_from_str_variants() {
    assert_eq!(ChatKind::from_telegram_type("private"), ChatKind::Private);
    assert_eq!(ChatKind::from_telegram_type("group"), ChatKind::Group);
    assert_eq!(
        ChatKind::from_telegram_type("supergroup"),
        ChatKind::Supergroup
    );
    assert_eq!(ChatKind::from_telegram_type("channel"), ChatKind::Channel);
    assert_eq!(
        ChatKind::from_telegram_type("something"),
        ChatKind::Unknown("something".into())
    );
}

#[test]
fn chat_kind_is_group_like() {
    assert!(!ChatKind::Private.is_group_like());
    assert!(ChatKind::Group.is_group_like());
    assert!(ChatKind::Supergroup.is_group_like());
    assert!(!ChatKind::Channel.is_group_like());
    assert!(!ChatKind::Unknown("x".into()).is_group_like());
}

#[test]
fn parse_group_message_is_group_flag() {
    let update = Update {
        update_id: 40,
        message: Some(make_text_message(
            60,
            make_chat(700, "supergroup"),
            make_user(800, Some("groupuser")),
            "hello group",
        )),
        callback_query: None,
    };

    let incoming = TelegramAdapter::parse_update(&update).unwrap();
    assert!(incoming.is_group);
    assert_eq!(incoming.chat_type, ChatKind::Supergroup);
}

// -----------------------------------------------------------------------
// Inline keyboard serialization tests
// -----------------------------------------------------------------------

#[test]
fn inline_keyboard_serialization() {
    let kb = InlineKeyboardMarkup {
        inline_keyboard: vec![
            vec![
                InlineKeyboardButton {
                    text: "Option A".into(),
                    callback_data: Some("a".into()),
                    url: None,
                },
                InlineKeyboardButton {
                    text: "Option B".into(),
                    callback_data: Some("b".into()),
                    url: None,
                },
            ],
            vec![InlineKeyboardButton {
                text: "Visit".into(),
                callback_data: None,
                url: Some("https://example.com".into()),
            }],
        ],
    };

    let json = serde_json::to_value(&kb).unwrap();
    let rows = json["inline_keyboard"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].as_array().unwrap().len(), 2);
    assert_eq!(rows[0][0]["text"], "Option A");
    assert_eq!(rows[0][0]["callback_data"], "a");
    assert!(rows[0][0].get("url").is_none());
    assert_eq!(rows[1][0]["url"], "https://example.com");
    assert!(rows[1][0].get("callback_data").is_none());
}

#[test]
fn inline_keyboard_deserialization() {
    let json = r#"{
            "inline_keyboard": [
                [{"text": "Go", "callback_data": "go"}],
                [{"text": "Link", "url": "https://x.com"}]
            ]
        }"#;
    let kb: InlineKeyboardMarkup = serde_json::from_str(json).unwrap();
    assert_eq!(kb.inline_keyboard.len(), 2);
    assert_eq!(kb.inline_keyboard[0][0].text, "Go");
    assert_eq!(kb.inline_keyboard[0][0].callback_data, Some("go".into()));
    assert_eq!(kb.inline_keyboard[1][0].url, Some("https://x.com".into()));
}

// -----------------------------------------------------------------------
// Media method routing tests
// -----------------------------------------------------------------------

#[test]
fn media_method_for_known_extensions() {
    assert_eq!(
        TelegramAdapter::media_method_for_extension("jpg"),
        ("sendPhoto", "photo")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("png"),
        ("sendPhoto", "photo")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("gif"),
        ("sendAnimation", "animation")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("mp4"),
        ("sendVideo", "video")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("mp3"),
        ("sendAudio", "audio")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("ogg"),
        ("sendVoice", "voice")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("pdf"),
        ("sendDocument", "document")
    );
    assert_eq!(
        TelegramAdapter::media_method_for_extension("zip"),
        ("sendDocument", "document")
    );
}

// -----------------------------------------------------------------------
// Sticker type serde tests
// -----------------------------------------------------------------------

#[test]
fn sticker_deserialization() {
    let json = r#"{
            "file_id": "stk_1",
            "file_unique_id": "stk_u1",
            "width": 512,
            "height": 512,
            "is_animated": true,
            "emoji": "🔥",
            "set_name": "HotPack"
        }"#;
    let sticker: Sticker = serde_json::from_str(json).unwrap();
    assert_eq!(sticker.file_id, "stk_1");
    assert_eq!(sticker.emoji, Some("🔥".into()));
    assert_eq!(sticker.is_animated, Some(true));
    assert_eq!(sticker.is_video, None);
}

#[test]
fn sticker_deserialization_minimal() {
    let json = r#"{"file_id": "s1", "file_unique_id": "su1"}"#;
    let sticker: Sticker = serde_json::from_str(json).unwrap();
    assert_eq!(sticker.file_id, "s1");
    assert!(sticker.width.is_none());
    assert!(sticker.emoji.is_none());
}

// -----------------------------------------------------------------------
// CallbackQuery serde tests
// -----------------------------------------------------------------------

#[test]
fn callback_query_deserialization() {
    let json = r#"{
            "id": "cq_999",
            "from": {"id": 123, "first_name": "Alice", "is_bot": false},
            "data": "pressed_ok",
            "chat_instance": "ci"
        }"#;
    let cq: CallbackQuery = serde_json::from_str(json).unwrap();
    assert_eq!(cq.id, "cq_999");
    assert_eq!(cq.from.id, 123);
    assert_eq!(cq.data, Some("pressed_ok".into()));
    assert!(cq.message.is_none());
}

// -----------------------------------------------------------------------
// Update deserialization with callback_query
// -----------------------------------------------------------------------

#[test]
fn update_with_callback_query_deser() {
    let json = r#"{
            "update_id": 50,
            "callback_query": {
                "id": "cq_1",
                "from": {"id": 1, "first_name": "Bob"},
                "message": {
                    "message_id": 77,
                    "chat": {"id": 88, "type": "private"},
                    "text": "Pick one"
                },
                "data": "choice_a"
            }
        }"#;
    let update: Update = serde_json::from_str(json).unwrap();
    assert!(update.message.is_none());
    let cq = update.callback_query.as_ref().unwrap();
    assert_eq!(cq.id, "cq_1");
    assert_eq!(cq.data, Some("choice_a".into()));
    assert_eq!(cq.message.as_ref().unwrap().message_id, 77);
}

// -----------------------------------------------------------------------
// TelegramResponse with parameters (rate limiting)
// -----------------------------------------------------------------------

#[test]
fn telegram_response_rate_limit_params() {
    let json = r#"{
            "ok": false,
            "description": "Too Many Requests: retry after 5",
            "parameters": {"retry_after": 5}
        }"#;
    let resp: TelegramResponse<serde_json::Value> = serde_json::from_str(json).unwrap();
    assert!(!resp.ok);
    assert_eq!(resp.parameters.as_ref().unwrap().retry_after, Some(5));
}

#[test]
fn telegram_response_error_uses_typed_retry_after() {
    let resp: TelegramResponse<serde_json::Value> = serde_json::from_value(serde_json::json!({
        "ok": false,
        "description": "Too Many Requests: retry after 37",
        "parameters": {"retry_after": 37}
    }))
    .unwrap();
    let err = TelegramAdapter::telegram_response_error("sendRichMessage", &resp);
    assert!(matches!(
        err,
        GatewayError::RateLimited {
            retry_after_secs: Some(37)
        }
    ));
}

#[test]
fn telegram_retry_after_parser_reads_bot_api_body() {
    assert_eq!(
        TelegramAdapter::retry_after_from_telegram_body(
            r#"{"ok":false,"parameters":{"retry_after":25}}"#
        ),
        Some(25)
    );
    assert_eq!(
        TelegramAdapter::retry_after_from_telegram_body(r#"{"ok":false}"#),
        None
    );
}

#[test]
fn telegram_response_no_params() {
    let json = r#"{"ok": true, "result": 42}"#;
    let resp: TelegramResponse<i32> = serde_json::from_str(json).unwrap();
    assert!(resp.ok);
    assert_eq!(resp.result, Some(42));
    assert!(resp.parameters.is_none());
}

#[test]
fn telegram_native_rich_reply_text_is_flattened() {
    let raw = serde_json::json!({
        "update_id": 1,
        "message": {
            "message_id": 8,
            "chat": {"id": 123, "type": "private"},
            "from": {"id": 7, "first_name": "User"},
            "text": "reply",
            "reply_to_message": {
                "message_id": 6,
                "chat": {"id": 123, "type": "private"},
                "from": {"id": 42, "first_name": "Hermes", "is_bot": true},
                "rich_message": {
                    "blocks": [
                        {"text": {"text": "Summary"}},
                        {
                            "items": [
                                {
                                    "label": "1.",
                                    "blocks": [{"text": ["First", " item"]}]
                                }
                            ]
                        }
                    ]
                }
            }
        }
    });
    let update: Update = serde_json::from_value(raw).expect("telegram update");
    let msg = update.message.expect("message");
    let reply = msg.reply_to_message.as_deref().expect("reply");

    assert_eq!(
        TelegramAdapter::extract_rich_reply_text(reply),
        Some("Summary\n1. First item".to_string())
    );
}

// -----------------------------------------------------------------------
// Chat deserialization with optional fields
// -----------------------------------------------------------------------

#[test]
fn chat_with_title() {
    let json = r#"{"id": 1, "type": "supergroup", "title": "My Group", "username": "mygrp"}"#;
    let chat: Chat = serde_json::from_str(json).unwrap();
    assert_eq!(chat.id, 1);
    assert_eq!(chat.chat_type, "supergroup");
    assert_eq!(chat.title, Some("My Group".into()));
    assert_eq!(chat.username, Some("mygrp".into()));
}

// -----------------------------------------------------------------------
// TelegramMessage with reply_to and sticker
// -----------------------------------------------------------------------

#[test]
fn telegram_message_full_deser() {
    let json = r#"{
            "message_id": 10,
            "chat": {"id": 1, "type": "private"},
            "from": {"id": 2, "first_name": "X", "is_bot": false},
            "text": "hi",
            "message_thread_id": 42,
            "reply_to_message": {
                "message_id": 5,
                "chat": {"id": 1, "type": "private"},
                "text": "earlier"
            }
        }"#;
    let msg: TelegramMessage = serde_json::from_str(json).unwrap();
    assert_eq!(msg.message_id, 10);
    assert_eq!(msg.message_thread_id, Some(42));
    let reply = msg.reply_to_message.as_ref().unwrap();
    assert_eq!(reply.message_id, 5);
    assert_eq!(reply.text, Some("earlier".into()));
}

// -----------------------------------------------------------------------
// Backoff state tests
// -----------------------------------------------------------------------

#[test]
fn backoff_doubling_capped() {
    let vals: Vec<u64> = {
        let mut v = Vec::new();
        let mut current = 0u64;
        for _ in 0..10 {
            current = if current == 0 {
                INITIAL_BACKOFF_MS
            } else {
                (current * 2).min(MAX_BACKOFF_MS)
            };
            v.push(current);
        }
        v
    };

    assert_eq!(vals[0], 1_000);
    assert_eq!(vals[1], 2_000);
    assert_eq!(vals[2], 4_000);
    assert_eq!(vals[3], 8_000);
    assert_eq!(vals[4], 16_000);
    assert_eq!(vals[5], 32_000);
    assert_eq!(vals[6], 60_000);
    assert_eq!(vals[7], 60_000);
}

#[test]
fn telegram_poll_request_timeout_adds_stall_grace() {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = std::env::var("TELEGRAM_POLL_STALL_GRACE_SECONDS").ok();
    std::env::set_var("TELEGRAM_POLL_STALL_GRACE_SECONDS", "4");

    let mut cfg = test_config();
    cfg.poll_timeout = 7;
    let adapter = test_adapter(cfg);

    assert_eq!(adapter.poll_request_timeout(), Duration::from_secs(11));

    match previous {
        Some(value) => std::env::set_var("TELEGRAM_POLL_STALL_GRACE_SECONDS", value),
        None => std::env::remove_var("TELEGRAM_POLL_STALL_GRACE_SECONDS"),
    }
}

#[test]
fn telegram_polling_threshold_marks_adapter_unhealthy() {
    let adapter = test_adapter(test_config());
    adapter.base.mark_running();
    adapter.consecutive_errors.store(3, Ordering::SeqCst);

    assert!(adapter.is_running());
    assert!(adapter.polling_reconnect_threshold_reached(3));

    adapter.mark_polling_unhealthy();

    assert!(!adapter.is_running());
}

#[tokio::test]
async fn telegram_delete_webhook_can_preserve_pending_updates() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/deleteWebhook"))
        .and(body_partial_json(serde_json::json!({
            "drop_pending_updates": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": true
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    adapter
        .delete_webhook(false)
        .await
        .expect("deleteWebhook should preserve pending updates when requested");
}

#[tokio::test]
async fn telegram_get_updates_advances_offset_after_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botfake_token_12345/getUpdates"))
        .and(body_partial_json(serde_json::json!({
            "offset": 0,
            "timeout": 30,
            "allowed_updates": ["message", "callback_query"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": [
                {"update_id": 41},
                {"update_id": 42}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut adapter = test_adapter(test_config());
    adapter.api_base = format!("{}/botfake_token_12345", server.uri());

    let updates = adapter.get_updates().await.expect("updates");

    assert_eq!(updates.len(), 2);
    assert_eq!(adapter.poll_offset.load(Ordering::SeqCst), 43);
}

#[test]
fn supported_document_type_from_extension_or_mime() {
    let doc_from_name = Document {
        file_id: "d1".into(),
        file_unique_id: None,
        file_name: Some("report.PDF".into()),
        mime_type: None,
        file_size: Some(1024),
    };
    assert!(TelegramAdapter::is_supported_document(&doc_from_name));

    let doc_from_mime = Document {
        file_id: "d2".into(),
        file_unique_id: None,
        file_name: None,
        mime_type: Some("text/plain".into()),
        file_size: Some(256),
    };
    assert!(TelegramAdapter::is_supported_document(&doc_from_mime));

    let doc_unsupported = Document {
        file_id: "d3".into(),
        file_unique_id: None,
        file_name: Some("archive.rar".into()),
        mime_type: Some("application/x-rar-compressed".into()),
        file_size: Some(1024),
    };
    assert!(!TelegramAdapter::is_supported_document(&doc_unsupported));

    let zip_doc = Document {
        file_id: "d4".into(),
        file_unique_id: None,
        file_name: Some("archive.zip".into()),
        mime_type: Some("application/zip".into()),
        file_size: Some(1024),
    };
    assert!(TelegramAdapter::is_supported_document(&zip_doc));

    let png_doc_from_mime = Document {
        file_id: "d5".into(),
        file_unique_id: None,
        file_name: None,
        mime_type: Some("image/png".into()),
        file_size: Some(1024),
    };
    assert!(TelegramAdapter::is_supported_document(&png_doc_from_mime));
}

#[test]
fn document_size_limit_check() {
    let small = Document {
        file_id: "d1".into(),
        file_unique_id: None,
        file_name: Some("ok.txt".into()),
        mime_type: Some("text/plain".into()),
        file_size: Some(1_024),
    };
    assert!(!TelegramAdapter::document_exceeds_size_limit(&small));

    let large = Document {
        file_id: "d2".into(),
        file_unique_id: None,
        file_name: Some("large.pdf".into()),
        mime_type: Some("application/pdf".into()),
        file_size: Some(TELEGRAM_MAX_DOCUMENT_SIZE_BYTES + 1),
    };
    assert!(TelegramAdapter::document_exceeds_size_limit(&large));

    let unknown_size = Document {
        file_id: "d3".into(),
        file_unique_id: None,
        file_name: Some("unknown.pdf".into()),
        mime_type: Some("application/pdf".into()),
        file_size: None,
    };
    assert!(TelegramAdapter::document_exceeds_size_limit(&unknown_size));
}

// -----------------------------------------------------------------------
// Bot mention tests
// -----------------------------------------------------------------------

fn make_adapter_with_bot_username(username: Option<&str>) -> TelegramAdapter {
    let mut config = test_config();
    config.bot_username = username.map(|s| s.to_string());
    TelegramAdapter::new(config).unwrap()
}

#[test]
fn is_mentioned_with_username() {
    let adapter = make_adapter_with_bot_username(Some("mybot"));
    assert!(adapter.is_mentioned_in("Hello @mybot how are you?"));
    assert!(!adapter.is_mentioned_in("Hello @otherbot"));
    assert!(!adapter.is_mentioned_in("Hello world"));
}

#[test]
fn is_mentioned_without_username_passthrough() {
    let adapter = make_adapter_with_bot_username(None);
    assert!(adapter.is_mentioned_in("anything"));
    assert!(adapter.is_mentioned_in(""));
}

#[test]
fn strip_mention_removes_at_mention() {
    let adapter = make_adapter_with_bot_username(Some("mybot"));
    assert_eq!(adapter.strip_mention("@mybot do something"), "do something");
    assert_eq!(
        adapter.strip_mention("hey @mybot please help"),
        "hey  please help"
    );
}

#[test]
fn strip_mention_no_username_passthrough() {
    let adapter = make_adapter_with_bot_username(None);
    assert_eq!(adapter.strip_mention("hello world"), "hello world");
}

// -----------------------------------------------------------------------
// User with is_bot field
// -----------------------------------------------------------------------

#[test]
fn user_is_bot_field() {
    let json = r#"{"id": 1, "first_name": "BotX", "is_bot": true}"#;
    let user: User = serde_json::from_str(json).unwrap();
    assert_eq!(user.is_bot, Some(true));
}

// -----------------------------------------------------------------------
// ChatMember deserialization
// -----------------------------------------------------------------------

#[test]
fn chat_member_deser() {
    let json = r#"{
            "status": "administrator",
            "user": {"id": 1, "first_name": "Admin"}
        }"#;
    let member: ChatMember = serde_json::from_str(json).unwrap();
    assert_eq!(member.status, "administrator");
    assert_eq!(member.user.id, 1);
}

// -----------------------------------------------------------------------
// Config serde
// -----------------------------------------------------------------------

#[test]
fn config_defaults() {
    let json = r#"{"token": "abc"}"#;
    let cfg: TelegramConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.token, "abc");
    assert!(cfg.polling);
    assert!(!cfg.parse_markdown);
    assert!(!cfg.parse_html);
    assert!(!cfg.disable_link_previews);
    assert!(!cfg.rich_messages);
    assert_eq!(cfg.poll_timeout, DEFAULT_POLL_TIMEOUT);
    assert_eq!(cfg.reply_to_mode, "first");
    assert!(cfg.webhook_secret.is_none());
    assert!(!cfg.reactions);
    assert!(cfg.bot_username.is_none());
    assert!(cfg.command_menu_enabled);
    assert_eq!(
        cfg.command_menu_max_commands,
        DEFAULT_TELEGRAM_COMMAND_MENU_MAX
    );
    assert!(cfg.command_menu_priority.is_empty());
    assert_eq!(cfg.command_menu_priority_mode, "prepend");
}

#[test]
fn config_with_bot_username() {
    let json = r#"{"token": "abc", "bot_username": "mybot"}"#;
    let cfg: TelegramConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.bot_username, Some("mybot".into()));
}

#[test]
fn config_with_reply_mode_webhook_secret_and_reactions() {
    let json = r#"{
            "token": "abc",
            "webhook_secret": "secret",
            "reply_to_mode": "all",
            "reactions": true,
            "disable_link_previews": true,
            "rich_messages": true,
            "command_menu_enabled": false,
            "command_menu_max_commands": 12,
            "command_menu_priority": ["status", "model"],
            "command_menu_priority_mode": "replace"
        }"#;
    let cfg: TelegramConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.webhook_secret.as_deref(), Some("secret"));
    assert_eq!(cfg.reply_to_mode, "all");
    assert!(cfg.reactions);
    assert!(cfg.disable_link_previews);
    assert!(cfg.rich_messages);
    assert!(!cfg.command_menu_enabled);
    assert_eq!(cfg.command_menu_max_commands, 12);
    assert_eq!(cfg.command_menu_priority, vec!["status", "model"]);
    assert_eq!(cfg.command_menu_priority_mode, "replace");
}
