use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn health_ok() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn metrics_prometheus_has_counters() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let s = String::from_utf8(body.to_vec()).unwrap();
    assert!(s.contains("hermes_llm_requests_total"));
}

#[tokio::test]
async fn command_help_runs_through_gateway() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let payload = serde_json::json!({ "command": "/help" });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/commands")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["accepted"], true);
    let out = v["output"].as_str().unwrap();
    assert!(
        out.to_lowercase().contains("help") || out.contains('/'),
        "unexpected output: {}",
        out
    );
}

#[tokio::test]
async fn billing_tier_mapping_stub() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/v1/billing/tier-mapping")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert!(v["mappings"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn billing_quota_stub() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/billing/quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["tier"], "free");
    assert_eq!(v["effective_provider_tier"], "economic");
}

#[tokio::test]
async fn compat_api_status_has_desktop_fields() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["gateway_running"], true);
    assert_eq!(v["status"], "running");
    assert!(v["hermes_home"].is_string());
    assert!(v["config_path"].is_string());
    assert!(v["active_sessions"].as_i64().is_some());
}

#[tokio::test]
async fn compat_api_messaging_platforms_shape() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/messaging/platforms")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["platforms"].is_array());
}
