//! Wiremock tests for Discord GET /gateway (R-01, R-02).

use hermes_gateway::platforms::discord::fetch_gateway_url_at;
use hermes_core::errors::GatewayError;

#[tokio::test]
async fn r01_fetch_gateway_returns_url() {
    let mock_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v10/gateway"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "url": "wss://gateway.discord.gg/?v=10&encoding=json"
            })),
        )
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let api_base = format!("{}/api/v10", mock_server.uri());
    let gateway_url =
        fetch_gateway_url_at(&client, "test-token", &api_base)
            .await
            .expect("gateway url");
    assert!(gateway_url.starts_with("wss://"));
}

#[tokio::test]
async fn r02_fetch_gateway_errors_on_401() {
    let mock_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v10/gateway"))
        .respond_with(wiremock::ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let api_base = format!("{}/api/v10", mock_server.uri());
    let err = fetch_gateway_url_at(&client, "bad-token", &api_base)
        .await
        .expect_err("401 should fail");
    match err {
        GatewayError::ConnectionFailed(msg) => assert!(msg.contains("401")),
        other => panic!("unexpected error: {other:?}"),
    }
}
