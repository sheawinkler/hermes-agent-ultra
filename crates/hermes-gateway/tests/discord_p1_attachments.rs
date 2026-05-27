//! P1.2 attachment parsing and attachment-only inbound acceptance.

#![cfg(feature = "discord")]

use hermes_gateway::platforms::discord::{
    parse_attachments, parse_message_create_raw, should_accept_message, ChannelIdSet,
    DiscordInboundConfig,
};

#[test]
fn a01_parse_message_attachments() {
    let data = serde_json::json!({
        "id": "m1",
        "channel_id": "c1",
        "guild_id": "g1",
        "content": "",
        "author": { "id": "u1", "bot": false },
        "attachments": [{
            "id": "att1",
            "filename": "photo.png",
            "content_type": "image/png",
            "url": "https://cdn.discordapp.com/attachments/1/2/photo.png",
            "size": 1024
        }]
    });
    let raw = parse_message_create_raw(&data).expect("parse");
    assert_eq!(raw.attachments.len(), 1);
    assert_eq!(raw.attachments[0].filename, "photo.png");
    assert_eq!(raw.attachments[0].content_type.as_deref(), Some("image/png"));
}

#[test]
fn a02_parse_attachments_helper() {
    let value = serde_json::json!([{
        "id": "99",
        "filename": "doc.pdf",
        "content_type": "application/pdf",
        "url": "https://cdn.discordapp.com/doc.pdf",
        "size": 50
    }]);
    let list = parse_attachments(Some(&value));
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "99");
}

#[test]
fn a03_attachment_only_passes_filter_without_text() {
    let data = serde_json::json!({
        "id": "m2",
        "channel_id": "ch1",
        "guild_id": "g1",
        "content": "",
        "author": { "id": "u1", "bot": false },
        "attachments": [{
            "id": "a1",
            "filename": "pic.png",
            "content_type": "image/png",
            "url": "https://cdn.discordapp.com/pic.png",
            "size": 10
        }]
    });
    let raw = parse_message_create_raw(&data).unwrap();
    let cfg = DiscordInboundConfig {
        require_mention: false,
        bot_user_id: Some("bot99".into()),
        free_response_channels: ChannelIdSet::new(),
        allowed_channels: ChannelIdSet::new(),
        ignored_channels: ChannelIdSet::new(),
        thread_participation: ChannelIdSet::new(),
    };
    assert!(should_accept_message(&raw, &cfg));
}
