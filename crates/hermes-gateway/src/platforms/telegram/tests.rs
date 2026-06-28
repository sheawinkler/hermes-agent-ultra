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

include!("tests/update_media_config.rs");
