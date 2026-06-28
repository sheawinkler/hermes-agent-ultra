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
