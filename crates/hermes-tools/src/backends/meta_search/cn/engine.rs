//! CN HTML engine trait and shared fetch helpers.

use std::time::Instant;

use reqwest::Client;
use tracing::debug;

use crate::backends::meta_search::{ParseError, SearchHit};
use crate::backends::meta_search::http_client::{BROWSER_ACCEPT_HTML, MAX_CN_HTML_BYTES};
use hermes_core::ToolError;

/// HTML search engine with separated fetch and parse for testability.
pub trait CnHtmlEngine: Send + Sync {
    fn id(&self) -> &'static str;
    fn production_base(&self) -> &'static str;
    fn search_path_and_query(&self, query: &str) -> String;
    fn parse_html(&self, html: &str) -> Result<Vec<SearchHit>, ParseError>;

    fn build_url(&self, query: &str, base_override: Option<&str>) -> String {
        let path_q = self.search_path_and_query(query);
        if path_q.starts_with("http://") || path_q.starts_with("https://") {
            return path_q;
        }
        match base_override {
            Some(base) => format!("{base}/{}{}", self.id(), path_q),
            None => format!("{}{}", self.production_base(), path_q),
        }
    }
}

pub async fn fetch_cn_html(client: &Client, url: &str) -> Result<String, ToolError> {
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, BROWSER_ACCEPT_HTML)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("CN search HTTP failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ToolError::ExecutionFailed(format!(
            "CN search HTTP error ({status})"
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("CN search read body failed: {e}")))?;
    let slice = if bytes.len() > MAX_CN_HTML_BYTES {
        // Sogou/Bing HTML can exceed the cap while still containing parseable results
        // in the head of the document; failing the whole engine was dropping CN results.
        tracing::warn!(
            bytes = bytes.len(),
            cap = MAX_CN_HTML_BYTES,
            "CN search HTML truncated before parse"
        );
        &bytes[..MAX_CN_HTML_BYTES]
    } else {
        &bytes
    };
    String::from_utf8(slice.to_vec()).map_err(|e| ToolError::ExecutionFailed(format!(
        "CN search response not UTF-8: {e}"
    )))
}

pub async fn run_cn_engine(
    engine: &dyn CnHtmlEngine,
    client: &Client,
    query: &str,
    limit: usize,
    base_override: Option<&str>,
) -> Result<Vec<SearchHit>, ToolError> {
    let started = Instant::now();
    let url = engine.build_url(query, base_override);
    debug!(engine = engine.id(), url = %url, "cn engine fetch start");
    let html = fetch_cn_html(client, &url).await?;
    let mut hits = engine
        .parse_html(&html)
        .map_err(|e| ToolError::ExecutionFailed(format!("{} parse failed: {e}", engine.id())))?;
    hits.truncate(limit.max(1));
    debug!(
        engine = engine.id(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        count = hits.len(),
        "cn engine fetch done"
    );
    Ok(hits)
}
