//! Unit-style tests for meta_search public APIs (avoids lib test compile issues elsewhere).

use std::sync::{Mutex, OnceLock};

use hermes_tools::backends::meta_search::config::{CnEngineKind, MetaSearchConfig};
use hermes_tools::backends::meta_search::ddgs::{
    ddgs_backend_priority, ddgs_http_timeout_secs, ddgs_region_from_env,
};
use hermes_tools::backends::meta_search::merge::merge_and_rank;
use hermes_tools::backends::meta_search::query_locale::query_has_cjk;
use hermes_tools::backends::meta_search::SearchHit;

fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env test lock")
}

struct EnvGuard {
    keys: Vec<&'static str>,
}

impl EnvGuard {
    fn meta_search_keys() -> Self {
        for k in [
            "HERMES_CN_SEARCH_ENGINES",
            "HERMES_CN_SEARCH_TIMEOUT_SECS",
            "HERMES_META_SEARCH_TIMEOUT_SECS",
            "HERMES_META_SEARCH_CN_WEIGHT",
            "HERMES_CN_SEARCH_BASE_URL_OVERRIDE",
            "HERMES_DDGS_BACKENDS",
            "HERMES_DDGS_REGION",
            "HERMES_DDGS_TIMEOUT_SECS",
        ] {
            hermes_core::test_env::remove_var(k);
        }
        Self {
            keys: vec![
                "HERMES_CN_SEARCH_ENGINES",
                "HERMES_CN_SEARCH_TIMEOUT_SECS",
                "HERMES_META_SEARCH_TIMEOUT_SECS",
                "HERMES_META_SEARCH_CN_WEIGHT",
                "HERMES_CN_SEARCH_BASE_URL_OVERRIDE",
                "HERMES_DDGS_BACKENDS",
                "HERMES_DDGS_REGION",
                "HERMES_DDGS_TIMEOUT_SECS",
            ],
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for k in &self.keys {
            hermes_core::test_env::remove_var(k);
        }
    }
}

fn hit(source: &str, url: &str, title: &str) -> SearchHit {
    SearchHit::new(title, url, "desc", source)
}

#[test]
fn query_locale_latin_only() {
    assert!(!query_has_cjk("hello world"));
    assert!(!query_has_cjk("Rust programming"));
}

#[test]
fn query_locale_chinese() {
    assert!(query_has_cjk("Rust 编程"));
    assert!(query_has_cjk("人工智能"));
}

#[test]
fn query_locale_mixed_and_emoji() {
    assert!(query_has_cjk("AI 新闻 2026"));
    assert!(!query_has_cjk("🚀 rocket"));
    assert!(!query_has_cjk(""));
}

#[test]
fn config_defaults_include_sogou_and_bing_cn() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    let cfg = MetaSearchConfig::from_env();
    assert_eq!(
        cfg.cn_engines,
        vec![CnEngineKind::Sogou, CnEngineKind::BingCn]
    );
    assert_eq!(cfg.cn_timeout_secs, 8);
    assert_eq!(cfg.global_timeout_secs, 12);
}

#[test]
fn config_empty_cn_engines_env_disables_cn() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    hermes_core::test_env::set_var("HERMES_CN_SEARCH_ENGINES", "");
    let cfg = MetaSearchConfig::from_env();
    assert!(cfg.cn_engines.is_empty());
}

#[test]
fn config_parses_engine_list_and_filters_unknown() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    hermes_core::test_env::set_var("HERMES_CN_SEARCH_ENGINES", "sogou,unknown,bing_cn");
    let cfg = MetaSearchConfig::from_env();
    assert_eq!(
        cfg.cn_engines,
        vec![CnEngineKind::Sogou, CnEngineKind::BingCn]
    );
}

#[test]
fn config_timeout_clamp() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    hermes_core::test_env::set_var("HERMES_CN_SEARCH_TIMEOUT_SECS", "999");
    hermes_core::test_env::set_var("HERMES_META_SEARCH_TIMEOUT_SECS", "5");
    let cfg = MetaSearchConfig::from_env();
    assert_eq!(cfg.cn_timeout_secs, 30);
    assert_eq!(cfg.global_timeout_secs, 5);
}

#[test]
fn config_cn_weight_override() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    hermes_core::test_env::set_var("HERMES_META_SEARCH_CN_WEIGHT", "2.0");
    let cfg = MetaSearchConfig::from_env();
    assert_eq!(cfg.cn_weight, 2.0);
}

#[test]
fn merge_dedups_by_url_and_title() {
    let batches = vec![
        vec![hit("sogou", "https://a.com", "Title")],
        vec![hit("bing_cn", "https://a.com", "Title")],
        vec![hit("ddgs_lite", "https://b.com", "Other")],
    ];
    let merged = merge_and_rank(batches, 10, true, 1.25);
    assert_eq!(merged.len(), 2);
}

#[test]
fn merge_cn_weight_prefers_sogou_over_ddgs() {
    let batches = vec![
        vec![hit("ddgs_lite", "https://ddg.com", "DDG")],
        vec![hit("sogou", "https://sg.com", "SG")],
    ];
    let merged = merge_and_rank(batches, 2, true, 2.0);
    assert_eq!(merged[0].source, "sogou");
}

#[test]
fn merge_limit_truncates() {
    let batches = vec![
        hit("sogou", "https://1.com", "A"),
        hit("sogou", "https://2.com", "B"),
        hit("sogou", "https://3.com", "C"),
    ]
    .into_iter()
    .map(|h| vec![h])
    .collect();
    assert_eq!(merge_and_rank(batches, 2, false, 1.0).len(), 2);
}

#[test]
fn merge_empty_input() {
    assert!(merge_and_rank(vec![], 5, false, 1.0).is_empty());
}

#[test]
fn ddgs_backend_priority_defaults_to_fast_backends() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    let backends = ddgs_backend_priority();
    assert_eq!(backends.len(), 4);
    assert_eq!(backends[0], ddgs::TextBackend::Lite);
    assert_eq!(backends[1], ddgs::TextBackend::Html);
}

#[test]
fn ddgs_backend_priority_parses_env_list() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    hermes_core::test_env::set_var("HERMES_DDGS_BACKENDS", "lite,yandex,not-a-backend");
    let backends = ddgs_backend_priority();
    assert_eq!(backends.len(), 2);
    assert_eq!(backends[0], ddgs::TextBackend::Lite);
    assert_eq!(backends[1], ddgs::TextBackend::Yandex);
}

#[test]
fn ddgs_region_cn_default() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    assert_eq!(ddgs_region_from_env(), ddgs::Region::CnZh);
}

#[test]
fn ddgs_timeout_default_is_eight_seconds() {
    let _lock = env_test_lock();
    let _g = EnvGuard::meta_search_keys();
    assert_eq!(ddgs_http_timeout_secs(), 8);
}
