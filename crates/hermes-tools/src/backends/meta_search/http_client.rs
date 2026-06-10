//! Shared HTTP client for meta-search HTML fetches.

use reqwest::Client;
use std::time::Duration;

pub const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

pub const BROWSER_ACCEPT_HTML: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

/// Max HTML body bytes read per CN engine response.
pub const MAX_CN_HTML_BYTES: usize = 512_000;

pub fn build_meta_search_client(default_timeout_secs: u64) -> Client {
    Client::builder()
        .timeout(Duration::from_secs(default_timeout_secs.max(1)))
        .user_agent(BROWSER_USER_AGENT)
        .build()
        .unwrap_or_else(|_| Client::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_looks_like_chrome() {
        assert!(BROWSER_USER_AGENT.contains("Chrome/"));
    }
}
