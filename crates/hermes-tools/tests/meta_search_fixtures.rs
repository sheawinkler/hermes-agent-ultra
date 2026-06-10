//! Parser fixture tests (no network).

use hermes_tools::backends::meta_search::cn::bing_cn::BingCnEngine;
use hermes_tools::backends::meta_search::cn::engine::CnHtmlEngine;
use hermes_tools::backends::meta_search::cn::sogou::SogouEngine;
use std::fs;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/meta_search")
        .join(name);
    fs::read_to_string(path).expect("read fixture")
}

#[test]
fn sogou_fixture_parses_two_results() {
    let hits = SogouEngine
        .parse_html(&fixture("sogou_results.html"))
        .expect("parse");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].source, "sogou");
}

#[test]
fn sogou_empty_fixture() {
    assert!(
        SogouEngine
            .parse_html(&fixture("sogou_empty.html"))
            .expect("parse")
            .is_empty()
    );
}

#[test]
fn bing_cn_fixture_parses_two_results() {
    let hits = BingCnEngine
        .parse_html(&fixture("bing_cn_results.html"))
        .expect("parse");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].source, "bing_cn");
}
