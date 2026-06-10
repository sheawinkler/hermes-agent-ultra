//! Meta-search wiremock integration tests (no production network for CN engines).

use std::sync::{Mutex, OnceLock};

use hermes_tools::backends::meta_search::orchestrator::meta_search;
use hermes_tools::backends::web::DdgsSearchBackend;
use hermes_tools::tools::web::WebSearchBackend;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/meta_search")
        .join(name);
    fs::read_to_string(path).expect("read fixture")
}

struct EnvGuard;

impl EnvGuard {
    fn clear() -> Self {
        for k in ENV_KEYS {
            hermes_core::test_env::remove_var(k);
        }
        Self
    }

    fn set(key: &'static str, value: &str) {
        hermes_core::test_env::set_var(key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for k in ENV_KEYS {
            hermes_core::test_env::remove_var(k);
        }
    }
}

fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env test lock")
}

const ENV_KEYS: &[&str] = &[
    "HERMES_CN_SEARCH_ENGINES",
    "HERMES_CN_SEARCH_BASE_URL_OVERRIDE",
    "HERMES_META_SEARCH_DDGS_DISABLED",
    "HERMES_CN_SEARCH_TIMEOUT_SECS",
    "HERMES_META_SEARCH_TIMEOUT_SECS",
];

async fn mount_cn_mocks(server: &MockServer) {
    let sogou_html = fixture("sogou_results.html");
    let bing_html = fixture("bing_cn_results.html");

    Mock::given(method("GET"))
        .and(path_regex(r"/sogou/web.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sogou_html))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/bing_cn/search.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bing_html))
        .mount(server)
        .await;
}

fn parse_json(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid json")
}

#[tokio::test]
async fn cjk_query_merges_sogou_and_bing_cn() {
    let _lock = env_test_lock();
    let _g = EnvGuard::clear();
    let server = MockServer::start().await;
    mount_cn_mocks(&server).await;

    EnvGuard::set("HERMES_CN_SEARCH_BASE_URL_OVERRIDE", &server.uri());
    EnvGuard::set("HERMES_CN_SEARCH_ENGINES", "sogou,bing_cn");
    EnvGuard::set("HERMES_META_SEARCH_DDGS_DISABLED", "1");

    let raw = meta_search("Rust 编程", 5)
        .await
        .expect("meta_search ok");
    let parsed = parse_json(&raw);
    assert_eq!(parsed["success"], true);
    let web = parsed["data"]["web"].as_array().expect("web array");
    assert!(web.len() >= 2);
    let sources: Vec<&str> = web
        .iter()
        .filter_map(|r| r.get("source").and_then(Value::as_str))
        .collect();
    assert!(sources.iter().any(|s| *s == "sogou"));
    assert!(sources.iter().any(|s| *s == "bing_cn"));
}

#[tokio::test]
async fn latin_query_does_not_hit_cn_mock_paths() {
    let _lock = env_test_lock();
    let _g = EnvGuard::clear();
    let server = MockServer::start().await;
    mount_cn_mocks(&server).await;

    EnvGuard::set("HERMES_CN_SEARCH_BASE_URL_OVERRIDE", &server.uri());
    EnvGuard::set("HERMES_CN_SEARCH_ENGINES", "sogou,bing_cn");

    let backend = DdgsSearchBackend::new();
    let raw = backend.search("hello world", 3, None).await.expect("search");
    let parsed = parse_json(&raw);
    let attempts = parsed["_trace"]["attempts"].as_array();
    if let Some(arr) = attempts {
        assert!(!arr.iter().any(|a| a["engine"] == "sogou"));
        assert!(!arr.iter().any(|a| a["engine"] == "bing_cn"));
    }
}

#[tokio::test]
async fn single_engine_error_still_returns_partial_results() {
    let _lock = env_test_lock();
    let _g = EnvGuard::clear();
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"/sogou/web.*"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"/bing_cn/search.*"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("bing_cn_results.html")))
        .mount(&server)
        .await;

    EnvGuard::set("HERMES_CN_SEARCH_BASE_URL_OVERRIDE", &server.uri());
    EnvGuard::set("HERMES_CN_SEARCH_ENGINES", "sogou,bing_cn");
    EnvGuard::set("HERMES_META_SEARCH_DDGS_DISABLED", "1");

    let raw = meta_search("人工智能", 5).await.expect("meta_search");
    let parsed = parse_json(&raw);
    assert_eq!(parsed["success"], true);
    let attempts = parsed["_trace"]["attempts"]
        .as_array()
        .expect("attempts");
    assert!(attempts.iter().any(|a| a["engine"] == "sogou" && a["status"] == "error"));
    assert!(attempts.iter().any(|a| a["engine"] == "bing_cn" && a["status"] == "ok"));
}

#[tokio::test]
async fn empty_cn_engines_env_skips_cn_mock_server() {
    let _lock = env_test_lock();
    let _g = EnvGuard::clear();
    let server = MockServer::start().await;
    mount_cn_mocks(&server).await;

    EnvGuard::set("HERMES_CN_SEARCH_BASE_URL_OVERRIDE", &server.uri());
    EnvGuard::set("HERMES_CN_SEARCH_ENGINES", "");
    EnvGuard::set("HERMES_META_SEARCH_DDGS_DISABLED", "1");

    let _ = meta_search("中文查询", 3).await;
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests.len(),
        0,
        "CN engines disabled — mock server must not be contacted"
    );
}

#[tokio::test]
async fn all_cn_engines_fail_returns_error_with_trace() {
    let _lock = env_test_lock();
    let _g = EnvGuard::clear();
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"/sogou/web.*"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/bing_cn/search.*"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    EnvGuard::set("HERMES_CN_SEARCH_BASE_URL_OVERRIDE", &server.uri());
    EnvGuard::set("HERMES_CN_SEARCH_ENGINES", "sogou,bing_cn");
    EnvGuard::set("HERMES_META_SEARCH_DDGS_DISABLED", "1");

    let raw = meta_search("测试失败", 3).await.expect("meta_search");
    let parsed = parse_json(&raw);
    assert_eq!(parsed["success"], false);
    assert!(parsed["_trace"]["attempts"].is_array());
}
