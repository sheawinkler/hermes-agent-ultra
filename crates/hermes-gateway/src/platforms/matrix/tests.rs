use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn test_markdown_to_html_bold() {
    let html = markdown_to_html("hello **world**");
    assert!(html.contains("<strong>world</strong>"));
}

#[test]
fn test_markdown_to_html_italic() {
    let html = markdown_to_html("hello *world*");
    assert!(html.contains("<em>world</em>"));
}

#[test]
fn test_markdown_to_html_inline_code() {
    let html = markdown_to_html("use `foo()` here");
    assert!(html.contains("<code>foo()</code>"));
}

#[test]
fn test_markdown_to_html_link() {
    let html = markdown_to_html("[click](https://example.com)");
    assert!(html.contains(r#"<a href="https://example.com">click</a>"#));
}

#[test]
fn test_markdown_to_html_escapes_raw_html() {
    let html = markdown_to_html(r#"hi <img src=x onerror=alert(1)> <script>x</script>"#);

    assert!(!html.contains("<img"));
    assert!(!html.contains("<script"));
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(html.contains("&lt;script&gt;x&lt;/script&gt;"));
}

#[test]
fn test_markdown_to_html_rejects_unsafe_link_scheme() {
    let html = markdown_to_html("[click](javascript:alert(1)) [mail](mailto:a@example.com)");

    assert!(!html.contains("javascript:"));
    assert!(!html.contains("<a href=\"javascript:"));
    assert!(html.contains("click)"));
    assert!(html.contains(r#"<a href="mailto:a@example.com">mail</a>"#));
}

#[test]
fn test_markdown_to_html_escapes_link_label_and_href() {
    let html = markdown_to_html(r#"[<b>x</b>](https://example.com/?q="bad"&ok=1)"#);

    assert!(html.contains("&lt;b&gt;x&lt;/b&gt;"));
    assert!(html.contains(r#"href="https://example.com/?q=&quot;bad&quot;&amp;ok=1""#));
}

#[test]
fn test_mxc_to_http() {
    let url = mxc_to_http("https://matrix.org", "mxc://matrix.org/abc123");
    assert_eq!(
        url,
        Some("https://matrix.org/_matrix/media/v3/download/matrix.org/abc123".to_string())
    );
}

#[test]
fn test_mxc_to_http_invalid() {
    assert_eq!(mxc_to_http("https://matrix.org", "not-mxc"), None);
}

#[test]
fn test_mxc_to_http_trailing_slash() {
    let url = mxc_to_http("https://matrix.org/", "mxc://matrix.org/xyz");
    assert_eq!(
        url,
        Some("https://matrix.org/_matrix/media/v3/download/matrix.org/xyz".to_string())
    );
}

#[test]
fn remote_image_file_name_keeps_existing_extension() {
    let file_name = remote_image_file_name(
        "https://cdn.example.com/path/diagram.jpeg?token=abc",
        Some("image/jpeg"),
    );
    assert_eq!(file_name, "diagram.jpeg");
}

#[test]
fn remote_image_file_name_uses_content_type_extension_hint() {
    let file_name =
        remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/webp"));
    assert_eq!(file_name, "diagram.webp");
}

#[test]
fn normalized_image_content_type_strips_params() {
    assert_eq!(
        normalized_image_content_type(Some("image/png; charset=binary")).as_deref(),
        Some("image/png")
    );
    assert_eq!(normalized_image_content_type(Some("text/plain")), None);
}

#[test]
fn matrix_room_identity_uses_member_count_for_named_dm() {
    let identity = MatrixAdapter::classify_room_identity(
        "!dm:example.org",
        Some("Alice & Hermes".into()),
        None,
        Some(2),
        true,
    );

    assert_eq!(identity.chat_type, "dm");
    assert!(!identity.direct_conflict);
    assert_eq!(identity.display_name, "Alice & Hermes");
    assert_eq!(identity.server_name.as_deref(), Some("example.org"));
}

#[test]
fn matrix_room_identity_marks_stale_direct_room_conflict() {
    let identity = MatrixAdapter::classify_room_identity(
        "!room:example.org",
        Some("Project Room".into()),
        Some("#project:example.org".into()),
        Some(3),
        true,
    );

    assert_eq!(identity.chat_type, "room");
    assert!(identity.direct_conflict);
    assert_eq!(identity.joined_member_count, Some(3));
}

#[test]
fn matrix_room_identity_falls_back_to_direct_name_heuristic_without_count() {
    let unnamed_direct =
        MatrixAdapter::classify_room_identity("!legacy:example.org", None, None, None, true);
    assert_eq!(unnamed_direct.chat_type, "dm");

    let named_direct = MatrixAdapter::classify_room_identity(
        "!legacy-room:example.org",
        Some("Explicit Room".into()),
        None,
        None,
        true,
    );
    assert_eq!(named_direct.chat_type, "room");
    assert!(named_direct.direct_conflict);
}

#[tokio::test]
async fn test_parse_sync_events_messages() {
    let config = MatrixConfig {
        homeserver_url: "https://matrix.test".into(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    };
    let adapter = MatrixAdapter::new(config).unwrap();

    let sync = serde_json::json!({
        "rooms": {
            "join": {
                "!room:test": {
                    "timeline": {
                        "events": [
                            {
                                "type": "m.room.message",
                                "event_id": "$evt1",
                                "sender": "@user:test",
                                "content": {
                                    "msgtype": "m.text",
                                    "body": "hello"
                                }
                            },
                            {
                                "type": "m.reaction",
                                "event_id": "$evt2",
                                "sender": "@user:test",
                                "content": {
                                    "m.relates_to": {
                                        "rel_type": "m.annotation",
                                        "event_id": "$evt1",
                                        "key": "👍"
                                    }
                                }
                            }
                        ]
                    }
                }
            }
        }
    });

    let msgs = adapter.parse_sync_events(&sync).await;
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].body, "hello");
    assert_eq!(msgs[0].event_type, "m.room.message");
    assert_eq!(msgs[1].body, "👍");
    assert_eq!(msgs[1].event_type, "m.reaction");
}

#[tokio::test]
async fn test_parse_sync_events_edit() {
    let config = MatrixConfig {
        homeserver_url: "https://matrix.test".into(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    };
    let adapter = MatrixAdapter::new(config).unwrap();

    let sync = serde_json::json!({
        "rooms": {
            "join": {
                "!room:test": {
                    "timeline": {
                        "events": [{
                            "type": "m.room.message",
                            "event_id": "$edit1",
                            "sender": "@user:test",
                            "content": {
                                "msgtype": "m.text",
                                "body": "* edited",
                                "m.new_content": {
                                    "msgtype": "m.text",
                                    "body": "edited"
                                },
                                "m.relates_to": {
                                    "rel_type": "m.replace",
                                    "event_id": "$orig1"
                                }
                            }
                        }]
                    }
                }
            }
        }
    });

    let msgs = adapter.parse_sync_events(&sync).await;
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].is_edit);
    assert_eq!(msgs[0].body, "edited");
}

#[test]
fn test_parse_invites() {
    let config = MatrixConfig {
        homeserver_url: "https://matrix.test".into(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    };
    let adapter = MatrixAdapter::new(config).unwrap();

    let sync = serde_json::json!({
        "rooms": {
            "invite": {
                "!room_a:test": {},
                "!room_b:test": {}
            }
        }
    });

    let invites = adapter.parse_invites(&sync);
    assert_eq!(invites.len(), 2);
}

#[test]
fn parse_invites_filters_inviter_when_allowlist_is_configured() {
    let config = MatrixConfig {
        homeserver_url: "https://matrix.test".into(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    };
    let adapter = MatrixAdapter::new(config).unwrap();

    let sync = serde_json::json!({
        "rooms": {
            "invite": {
                "!trusted:test": {
                    "invite_state": {
                        "events": [{
                            "type": "m.room.member",
                            "state_key": "@bot:test",
                            "sender": "@trusted:test",
                            "content": {"membership": "invite"}
                        }]
                    }
                },
                "!attacker:test": {
                    "invite_state": {
                        "events": [{
                            "type": "m.room.member",
                            "state_key": "@bot:test",
                            "sender": "@attacker:test",
                            "content": {"membership": "invite"}
                        }]
                    }
                },
                "!unknown:test": {}
            }
        }
    });

    let mut invites = adapter.parse_invites_with_auth(&sync, &["@trusted:test".to_string()], false);
    invites.sort();
    assert_eq!(invites, vec!["!trusted:test".to_string()]);

    let mut allow_all =
        adapter.parse_invites_with_auth(&sync, &["@trusted:test".to_string()], true);
    allow_all.sort();
    assert_eq!(
        allow_all,
        vec![
            "!attacker:test".to_string(),
            "!trusted:test".to_string(),
            "!unknown:test".to_string(),
        ]
    );
}

#[tokio::test]
async fn test_parse_sync_encrypted_event_metadata() {
    let config = MatrixConfig {
        homeserver_url: "https://matrix.test".into(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    };
    let adapter = MatrixAdapter::new(config).unwrap();

    let sync = serde_json::json!({
        "rooms": {
            "join": {
                "!room:test": {
                    "timeline": {
                        "events": [{
                            "type": "m.room.encrypted",
                            "event_id": "$enc1",
                            "sender": "@user:test",
                            "content": {
                                "algorithm": "m.megolm.v1.aes-sha2"
                            }
                        }]
                    }
                }
            }
        }
    });

    let msgs = adapter.parse_sync_events(&sync).await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].event_type, "m.room.encrypted");
    assert!(msgs[0].body.contains("m.megolm.v1.aes-sha2"));
    assert!(adapter.e2ee.is_room_marked_encrypted("!room:test"));
}

#[test]
fn test_parse_decrypt_ffi_output_accepts_matrix_message_shape() {
    let out = MatrixAdapter::parse_decrypt_ffi_output(
        r#"{"type":"m.room.message","content":{"body":"hello from decrypt"}}"#,
    )
    .unwrap();
    assert_eq!(out.body, "hello from decrypt");
    assert_eq!(out.event_type, "m.room.message");
    assert!(!out.is_edit);
    assert!(out.relates_to.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn test_parse_sync_encrypted_event_uses_ffi_bridge() {
    let config = MatrixConfig {
        homeserver_url: "https://matrix.test".into(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    };
    let mut adapter = MatrixAdapter::new(config).unwrap();
    adapter.decrypt_ffi = Some(MatrixDecryptFfiConfig {
        command: "sh".to_string(),
        args: vec![
            "-lc".to_string(),
            "cat >/dev/null; printf '%s' '{\"body\":\"decrypted hello\",\"event_type\":\"m.room.message\"}'"
                .to_string(),
        ],
        timeout: Duration::from_millis(500),
    });

    let sync = serde_json::json!({
        "rooms": {
            "join": {
                "!room:test": {
                    "timeline": {
                        "events": [{
                            "type": "m.room.encrypted",
                            "event_id": "$enc2",
                            "sender": "@user:test",
                            "content": {
                                "algorithm": "m.megolm.v1.aes-sha2",
                                "ciphertext": "xyz"
                            }
                        }]
                    }
                }
            }
        }
    });

    let msgs = adapter.parse_sync_events(&sync).await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].event_type, "m.room.message");
    assert_eq!(msgs[0].body, "decrypted hello");
    assert!(adapter.e2ee.is_room_marked_encrypted("!room:test"));
}

#[tokio::test]
async fn test_parse_sync_encrypted_event_fallback_when_ffi_fails() {
    let config = MatrixConfig {
        homeserver_url: "https://matrix.test".into(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    };
    let mut adapter = MatrixAdapter::new(config).unwrap();
    adapter.decrypt_ffi = Some(MatrixDecryptFfiConfig {
        command: "/definitely-not-a-real-binary-hermes".to_string(),
        args: Vec::new(),
        timeout: Duration::from_millis(100),
    });

    let sync = serde_json::json!({
        "rooms": {
            "join": {
                "!room:test": {
                    "timeline": {
                        "events": [{
                            "type": "m.room.encrypted",
                            "event_id": "$enc3",
                            "sender": "@user:test",
                            "content": {
                                "algorithm": "m.megolm.v1.aes-sha2",
                                "session_id": "abc123"
                            }
                        }]
                    }
                }
            }
        }
    });

    let msgs = adapter.parse_sync_events(&sync).await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].event_type, "m.room.encrypted");
    assert!(msgs[0].body.contains("session_id=abc123"));
    assert!(adapter.e2ee.is_room_marked_encrypted("!room:test"));
}

#[tokio::test]
async fn test_e2ee_is_encrypted_room() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/_matrix/client/v3/rooms/room123/state/m.room.encryption/",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "algorithm": "m.megolm.v1.aes-sha2"
        })))
        .mount(&server)
        .await;

    let adapter = MatrixAdapter::new(MatrixConfig {
        homeserver_url: server.uri(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    })
    .unwrap();

    let encrypted = adapter.e2ee.is_encrypted_room("room123").await.unwrap();
    assert!(encrypted);
    assert!(adapter.e2ee.is_room_marked_encrypted("room123"));
}

#[tokio::test]
async fn test_e2ee_verify_device_keys() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/keys/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_keys": {
                "@alice:test": {
                    "ALDEVICE1": {
                        "keys": {"curve25519:ALDEVICE1": "abc"},
                        "algorithms": ["m.olm.v1.curve25519-aes-sha2"]
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    let adapter = MatrixAdapter::new(MatrixConfig {
        homeserver_url: server.uri(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    })
    .unwrap();

    let device_count = adapter
        .e2ee
        .verify_device_keys("@alice:test")
        .await
        .unwrap();
    assert_eq!(device_count, 1);
}

#[tokio::test]
async fn test_e2ee_share_room_keys_claims_one_time_keys() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/_matrix/client/v3/rooms/room123/members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "chunk": [
                {
                    "state_key": "@bot:test",
                    "content": {"membership": "join"}
                },
                {
                    "state_key": "@alice:test",
                    "content": {"membership": "join"}
                }
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/keys/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_keys": {
                "@alice:test": {
                    "ALDEVICE1": {
                        "keys": {"curve25519:ALDEVICE1": "abc"}
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/keys/claim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "one_time_keys": {
                "@alice:test": {
                    "ALDEVICE1": {"key": "otk"}
                }
            }
        })))
        .mount(&server)
        .await;

    let adapter = MatrixAdapter::new(MatrixConfig {
        homeserver_url: server.uri(),
        user_id: "@bot:test".into(),
        access_token: "tok".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    })
    .unwrap();

    let claimed = adapter.e2ee.share_room_keys("room123").await.unwrap();
    assert_eq!(claimed, 1);
    assert!(adapter.e2ee.is_room_marked_encrypted("room123"));
}
