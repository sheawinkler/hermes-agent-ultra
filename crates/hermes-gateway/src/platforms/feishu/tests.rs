#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event_v2() -> FeishuEvent {
        FeishuEvent {
            schema: Some("2.0".to_string()),
            header: Some(FeishuEventHeader {
                event_id: "ev_123".to_string(),
                event_type: "im.message.receive_v1".to_string(),
                create_time: Some("1234567890".to_string()),
                token: Some("test_token_abc".to_string()),
                app_id: Some("cli_xxx".to_string()),
                tenant_key: Some("tk_xxx".to_string()),
            }),
            event: Some(serde_json::json!({
                "sender": {
                    "sender_id": { "open_id": "ou_user1" }
                },
                "message": {
                    "message_id": "om_msg1",
                    "chat_id": "oc_chat1",
                    "chat_type": "group",
                    "message_type": "text",
                    "content": "{\"text\":\"@_user_1 hello world\"}",
                    "mentions": [{ "key": "@_user_1", "id": { "open_id": "ou_bot" } }]
                }
            })),
            challenge: None,
            token: None,
            event_type: None,
        }
    }

    fn sample_url_verification() -> FeishuEvent {
        FeishuEvent {
            schema: None,
            header: None,
            event: None,
            challenge: Some("challenge_abc".to_string()),
            token: Some("test_token_abc".to_string()),
            event_type: Some("url_verification".to_string()),
        }
    }

    #[test]
    fn verify_event_v2_correct_token() {
        let event = sample_event_v2();
        assert!(FeishuAdapter::verify_event(&event, "test_token_abc"));
    }

    #[test]
    fn verify_event_v2_wrong_token() {
        let event = sample_event_v2();
        assert!(!FeishuAdapter::verify_event(&event, "wrong_token"));
    }

    #[test]
    fn verify_url_verification_event() {
        let event = sample_url_verification();
        assert!(FeishuAdapter::verify_event(&event, "test_token_abc"));
    }

    #[test]
    fn parse_message_event_basic() {
        let event = sample_event_v2();
        let msg = FeishuAdapter::parse_message_event(event.event.as_ref().unwrap())
            .expect("should parse");
        assert_eq!(msg.message_id, "om_msg1");
        assert_eq!(msg.chat_id, "oc_chat1");
        assert_eq!(msg.chat_type, "group");
        assert_eq!(msg.sender_id.as_deref(), Some("ou_user1"));
        assert_eq!(msg.text, "hello world");
        assert!(msg.is_mention);
    }

    #[test]
    fn parse_message_event_dm() {
        let event_val = serde_json::json!({
            "sender": { "sender_id": { "open_id": "ou_user2" } },
            "message": {
                "message_id": "om_msg2",
                "chat_id": "oc_chat2",
                "chat_type": "p2p",
                "message_type": "text",
                "content": "{\"text\":\"direct message\"}"
            }
        });
        let msg = FeishuAdapter::parse_message_event(&event_val).expect("should parse");
        assert_eq!(msg.text, "direct message");
        assert!(!msg.is_mention);
        assert!(!FeishuAdapter::is_group_chat(&msg));
    }

    #[test]
    fn is_group_chat_true() {
        let event = sample_event_v2();
        let msg = FeishuAdapter::parse_message_event(event.event.as_ref().unwrap()).unwrap();
        assert!(FeishuAdapter::is_group_chat(&msg));
    }

    #[test]
    fn strip_mentions_basic() {
        assert_eq!(strip_mentions("@_user_123 hello"), "hello");
        assert_eq!(
            strip_mentions("hey @_user_1 and @_user_2 there"),
            "hey and there"
        );
        assert_eq!(strip_mentions("no mentions"), "no mentions");
    }

    #[test]
    fn post_element_builders() {
        let t = PostElement::text("hello");
        match t {
            PostElement::Text { ref text, .. } => assert_eq!(text, "hello"),
            _ => panic!("expected Text"),
        }

        let l = PostElement::link("click", "https://example.com");
        match l {
            PostElement::Link { ref text, ref href } => {
                assert_eq!(text, "click");
                assert_eq!(href, "https://example.com");
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn card_builder() {
        let card = FeishuCard::new()
            .with_header("Test Card", Some("blue"))
            .add_markdown("**bold** text")
            .add_hr()
            .add_div("plain text")
            .add_button("Click me", None, Some("primary"));

        assert!(card.header.is_some());
        assert_eq!(card.elements.len(), 4);
    }

    #[test]
    fn card_serialization_roundtrip() {
        let card = FeishuCard::new()
            .with_header("Title", None)
            .add_markdown("content");

        let json = serde_json::to_string(&card).expect("serialize");
        let _: FeishuCard = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn feishu_event_deserialize_url_verification() {
        let json = r#"{
            "challenge": "abc123",
            "token": "verify_tok",
            "type": "url_verification"
        }"#;
        let event: FeishuEvent = serde_json::from_str(json).expect("deserialize");
        assert_eq!(event.challenge.as_deref(), Some("abc123"));
        assert_eq!(event.token.as_deref(), Some("verify_tok"));
        assert_eq!(event.event_type.as_deref(), Some("url_verification"));
    }

    #[test]
    fn feishu_event_deserialize_v2() {
        let json = r#"{
            "schema": "2.0",
            "header": {
                "event_id": "ev_1",
                "event_type": "im.message.receive_v1",
                "token": "tok_abc"
            },
            "event": { "message": {} }
        }"#;
        let event: FeishuEvent = serde_json::from_str(json).expect("deserialize");
        assert_eq!(event.schema.as_deref(), Some("2.0"));
        assert!(event.header.is_some());
        let h = event.header.unwrap();
        assert_eq!(h.event_type, "im.message.receive_v1");
    }

    #[test]
    fn flatten_post_content_basic() {
        let content = serde_json::json!({
            "title": "My Title",
            "content": [
                [{ "tag": "text", "text": "Hello " }, { "tag": "a", "text": "link" }],
                [{ "tag": "text", "text": "Second line" }]
            ]
        });
        let result = FeishuAdapter::flatten_post_content(&content);
        assert!(result.contains("My Title"));
        assert!(result.contains("Hello link"));
        assert!(result.contains("Second line"));
    }

    #[test]
    fn remote_image_file_name_keeps_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/path/diagram.png?token=abc",
            Some("image/png"),
        );
        assert_eq!(file_name, "diagram.png");
    }

    #[test]
    fn remote_image_file_name_adds_extension_from_content_type() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/jpeg"));
        assert_eq!(file_name, "diagram.jpg");
    }

    #[test]
    fn image_fallback_text_with_caption() {
        let text = image_fallback_text("https://cdn.example.com/path/diagram", Some("Figure 1"));
        assert_eq!(text, "Figure 1\nhttps://cdn.example.com/path/diagram");
    }

    #[test]
    fn format_message_trims_whitespace() {
        assert_eq!(format_message("\n\nhello world\n"), "hello world");
        assert_eq!(format_message("  hello world  "), "hello world");
    }

    #[tokio::test]
    async fn stop_refuses_outbound_api_until_rearmed() {
        let adapter = FeishuAdapter::new(FeishuConfig {
            app_id: "cli_test_app".to_string(),
            app_secret: "secret".to_string(),
            verification_token: None,
            encrypt_key: None,
            proxy: AdapterProxyConfig::default(),
        })
        .expect("adapter");

        adapter.stop().await.expect("stop");

        let err = adapter
            .send_text("oc_chat", "hello after shutdown")
            .await
            .expect_err("send is refused while shutting down");
        let rendered = err.to_string();
        assert!(rendered.contains("Feishu adapter is shutting down"));
        assert!(rendered.contains("tenant token refresh"));

        adapter.rearm_runtime();
        assert!(!adapter.closing.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stopped_feishu_file_paths_do_not_touch_local_inputs() {
        let adapter = FeishuAdapter::new(FeishuConfig {
            app_id: "cli_test_app".to_string(),
            app_secret: "secret".to_string(),
            verification_token: None,
            encrypt_key: None,
            proxy: AdapterProxyConfig::default(),
        })
        .expect("adapter");

        adapter.stop().await.expect("stop");

        let err = adapter
            .send_file("oc_chat", "/definitely/missing/file.txt", None)
            .await
            .expect_err("shutdown guard wins before local file reads");
        let rendered = err.to_string();
        assert!(rendered.contains("Feishu adapter is shutting down"));
        assert!(rendered.contains("file send"));
    }
}
