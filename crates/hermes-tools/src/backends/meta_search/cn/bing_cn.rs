//! Bing China web search (HTTP HTML parse).
//!
//! Selector layout aligned with A3S-Lab/Search `engines/bing_china.rs`.

use scraper::{Html, Selector};

use super::engine::CnHtmlEngine;
use crate::backends::meta_search::{ParseError, SearchHit};

const RESULT_SEL: &str = "li.b_algo";
const TITLE_SEL: &str = "h2 a";
const SNIPPET_SEL: &str = ".b_caption p, .b_algoSlug";

pub struct BingCnEngine;

impl BingCnEngine {
    pub const ID: &'static str = "bing_cn";
    pub const BASE: &'static str = "https://cn.bing.com";
}

impl CnHtmlEngine for BingCnEngine {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn production_base(&self) -> &'static str {
        Self::BASE
    }

    fn search_path_and_query(&self, query: &str) -> String {
        format!(
            "/search?q={}",
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
            let url = title_elem.value().attr("href").unwrap_or_default().trim();
            if title.is_empty() || !is_http_url(url) {
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

fn is_http_url(raw: &str) -> bool {
    raw.starts_with("http://") || raw.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_html() {
        assert!(BingCnEngine.parse_html(" ").unwrap().is_empty());
    }

    #[test]
    fn parses_two_results() {
        let html = r#"
        <li class="b_algo">
            <h2><a href="https://www.rust-lang.org/">Rust Programming Language</a></h2>
            <div class="b_caption"><p>A language empowering everyone.</p></div>
        </li>
        <li class="b_algo">
            <h2><a href="https://doc.rust-lang.org/book/">The Rust Book</a></h2>
            <p class="b_algoSlug">The official Rust book.</p>
        </li>
        "#;
        let results = BingCnEngine.parse_html(html).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[1].title, "The Rust Book");
    }

    #[test]
    fn skips_non_http_urls() {
        let html = r#"
        <li class="b_algo"><h2><a href="/relative">Bad Link</a></h2></li>
        "#;
        assert!(BingCnEngine.parse_html(html).unwrap().is_empty());
    }

    #[test]
    fn skips_missing_title() {
        let html = r#"<li class="b_algo"><div>No title</div></li>"#;
        assert!(BingCnEngine.parse_html(html).unwrap().is_empty());
    }

    #[test]
    fn algo_slug_snippet() {
        let html = r#"
        <li class="b_algo">
            <h2><a href="https://example.com/">Example</a></h2>
            <p class="b_algoSlug">Snippet from algo slug.</p>
        </li>
        "#;
        let results = BingCnEngine.parse_html(html).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].description, "Snippet from algo slug.");
    }
}
