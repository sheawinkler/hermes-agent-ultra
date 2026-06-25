use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use tower::ServiceExt;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

async fn post_rpc(payload: serde_json::Value) -> serde_json::Value {
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/rpc")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

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
async fn rpc_project_facts_returns_structured_workspace_data() {
    let _ = tracing_subscriber::fmt::try_init();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname='rpc-facts'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("AGENTS.md"), "# rules").unwrap();

    let payload = serde_json::json!({
        "id": 1,
        "method": "project.facts",
        "params": { "cwd": repo.path() }
    });

    let v = post_rpc(payload).await;

    assert_eq!(v["id"], 1);
    assert_eq!(v["error"], serde_json::Value::Null);
    let facts = &v["result"]["facts"];
    assert_eq!(
        facts["root"],
        repo.path().canonicalize().unwrap().display().to_string()
    );
    assert!(facts["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "Cargo.toml"));
    assert!(facts["verifyCommands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "cargo test"));
    assert!(facts["contextFiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "AGENTS.md"));
}

#[tokio::test]
async fn rpc_verification_status_returns_passive_terminal_evidence() {
    let _ = tracing_subscriber::fmt::try_init();
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname='rpc-verification'\nversion='0.1.0'\n",
    )
    .unwrap();
    let _home = EnvGuard::set("HERMES_HOME", home.to_str().unwrap());
    hermes_tools::verification_evidence::record_terminal_result(
        "cargo   test -p hermes-http",
        Some(&repo),
        Some("sid-1"),
        0,
        "ok",
    )
    .unwrap();

    let payload = serde_json::json!({
        "id": "verify",
        "method": "verification.status",
        "params": { "cwd": repo, "session_id": "sid-1" }
    });

    let v = post_rpc(payload).await;

    assert_eq!(v["id"], "verify");
    assert_eq!(v["result"]["verification"]["status"], "passed");
    assert_eq!(
        v["result"]["verification"]["evidence"]["canonicalCommand"],
        "cargo test -p hermes-http"
    );
    assert_eq!(
        v["result"]["verification"]["evidence"]["outputPreview"],
        "ok"
    );
}

#[tokio::test]
async fn rpc_llm_oneshot_rejects_missing_prompt_without_provider_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let payload = serde_json::json!({
        "id": "missing",
        "method": "llm.oneshot",
        "params": {}
    });

    let v = post_rpc(payload).await;

    assert_eq!(v["id"], "missing");
    assert_eq!(v["error"]["code"], 4030);
    assert!(v["result"].is_null());
}
