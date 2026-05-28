//! P2-4 Discord inbound text batching + outbound split delay.

#![cfg(feature = "discord")]

use std::sync::Arc;
use std::time::Duration;

use hermes_gateway::gateway::IncomingMessage;
use hermes_gateway::platforms::discord::{deliver_inbounds, DiscordAdapter, DiscordConfig};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn text_batch_env_defaults_from_config() {
    let cfg = DiscordConfig::for_test("tok");
    assert_eq!(cfg.text_batch_delay_seconds, 0.0);
    assert_eq!(cfg.text_batch_split_delay_seconds, 0.0);
}

#[tokio::test]
async fn inbound_batch_merges_rapid_messages() {
    let mut config = DiscordConfig::for_test("tok");
    config.text_batch_delay_seconds = 0.05;
    config.text_batch_split_delay_seconds = 0.05;
    let adapter = DiscordAdapter::new(config).unwrap();
    let inner = Arc::clone(adapter.inner());

    let (tx, mut rx) = mpsc::channel(8);
    adapter.set_inbound_sender(tx).await;

    let a = IncomingMessage::new("discord", "ch1", "u1", "part one", false);
    let b = IncomingMessage::new("discord", "ch1", "u1", "part two", false);
    deliver_inbounds(&inner, vec![a, b]).await;

    tokio::time::sleep(Duration::from_millis(120)).await;
    let merged = rx.try_recv().expect("batched inbound");
    assert!(merged.text.contains("part one"));
    assert!(merged.text.contains("part two"));
}

#[tokio::test]
async fn outbound_split_delay_between_posts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"/api/v10/channels/.*/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "id": "m1", "channel_id": "ch1" })),
        )
        .expect(2)
        .mount(&server)
        .await;

    let mut config = DiscordConfig::for_test("test-token");
    config.rest_api_base = format!("{}/api/v10", server.uri());
    config.text_batch_split_delay_seconds = 0.15;
    let adapter = DiscordAdapter::new(config).unwrap();
    let content = "x".repeat(2001);
    let start = std::time::Instant::now();
    adapter
        .send_text_with_reply("ch1", &content, None)
        .await
        .expect("send ok");
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(100),
        "expected split delay between chunks, got {elapsed:?}"
    );
}
