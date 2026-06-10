//! Sogou web search (HTTP HTML parse).
//!
//! Selector layout aligned with A3S-Lab/Search `engines/sogou.rs`.

use scraper::{Html, Selector};

use super::engine::CnHtmlEngine;
use crate::backends::meta_search::{ParseError, SearchHit};

// Selectors (A3S-Lab/Search reference)
const RESULT_SEL: &str = "div.vrwrap, div.rb";
const TITLE_SEL: &str = "h3 a, a.vr-title, .vr-title a";
const SNIPPET_SEL: &str = ".str-text, .str_info, .space-txt";

pub struct SogouEngine;

impl SogouEngine {
    pub const ID: &'static str = "sogou";
    pub const BASE: &'static str = "https://www.sogou.com";
}

impl CnHtmlEngine for SogouEngine {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn production_base(&self) -> &'static str {
        Self::BASE
    }

    fn search_path_and_query(&self, query: &str) -> String {
        format!(
            "/web?query={}",
            url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
        )
    }

    fn parse_html(&self, html: &str) -> Result<Vec<SearchHit>, ParseError> {
        let document = Html::parse_document(html);
        let result_sel = Selector::parse(RESULT_SEL)
            .map_err(|e| ParseError(format!("invalid result selector: {e}")))?;
        let title_sel = Selector::parse(TITLE_SEL)
            .map_err(|e| ParseError(format!("invalid title selector: {e}")))?;
        let snippet_sel = Selector::parse(SNIPPET_SEL)
            .map_err(|e| ParseError(format!("invalid snippet selector: {e}")))?;

        let mut results = Vec::new();
        for element in document.select(&result_sel) {
            let title_elem = match element.select(&title_sel).next() {
                Some(el) => el,
                None => continue,
            };
            let title = title_elem.text().collect::<String>().trim().to_string();
            let raw_url = title_elem.value().attr("href").unwrap_or_default();
            let url = normalize_sogou_url(raw_url);
            if url.is_empty() || title.is_empty() {
                continue;
            }
            let description = element
                .select(&snippet_sel)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .unwrap_or_default();
            results.push(SearchHit::new(title, url, description, Self::ID));
        }
        Ok(results)
    }
}

fn normalize_sogou_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }
    if trimmed.starts_with('/') {
        return format!("{}{trimmed}", SogouEngine::BASE);
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_fixture(html: &str) -> Vec<SearchHit> {
        SogouEngine.parse_html(html).expect("parse")
    }

    #[test]
    fn empty_html() {
        assert!(parse_fixture(" ").is_empty());
    }

    #[test]
    fn parses_results_with_relative_url() {
        let html = r#"
        <div class="vrwrap">
            <h3><a href="/link?url=abc123">Rust Programming</a></h3>
            <div class="str-text">A systems programming language.</div>
        </div>
        <div class="rb">
            <h3><a class="vr-title" href="https://example.com/page">Example Page</a></h3>
            <span class="str_info">Some description here.</span>
        </div>
        "#;
        let results = parse_fixture(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming");
        assert_eq!(results[0].url, "https://www.sogou.com/link?url=abc123");
        assert_eq!(results[0].description, "A systems programming language.");
        assert_eq!(results[1].title, "Example Page");
        assert_eq!(results[1].url, "https://example.com/page");
    }

    #[test]
    fn skips_missing_title() {
        let html = r#"<div class="vrwrap"><div class="str-text">no link</div></div>"#;
        assert!(parse_fixture(html).is_empty());
    }

    #[test]
    fn build_url_encodes_query() {
        let url = SogouEngine.build_url("Rust 编程", None);
        assert!(url.starts_with("https://www.sogou.com/web?query="));
        assert!(url.contains("%"));
    }
}
