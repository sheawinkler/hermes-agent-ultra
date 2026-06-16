//! HTTP transport with auth injection, tracing headers, and retry policy.

use std::time::Duration;

use hermes_config::ServerConfig;
use reqwest::header::{
    AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT,
};
use reqwest::{Client, Method, Response};
use tracing::debug;

use crate::error::ServerClientError;
use crate::session::ServerSession;

const REQUEST_ID_HEADER: &str = "x-request-id";
const CLIENT_VERSION_HEADER: &str = "x-client-version";
const LEGACY_TOKEN_HEADER: &str = "token";

/// Shared HTTP client for server auth and LLM calls.
#[derive(Clone)]
pub struct HttpTransport {
    client: Client,
    base_url: String,
    user_agent: String,
}

impl HttpTransport {
    pub fn new(config: &ServerConfig) -> Result<Self, ServerClientError> {
        if config.enabled && config.base_url.trim().is_empty() {
            return Err(ServerClientError::MissingBaseUrl);
        }
        Self::from_base_url(&config.base_url, config.llm.request_timeout_seconds)
    }

    pub fn from_base_url(base_url: &str, timeout_secs: u64) -> Result<Self, ServerClientError> {
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(ServerClientError::MissingBaseUrl);
        }

        let timeout = Duration::from_secs(timeout_secs.max(1));
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ServerClientError::Http(format!("build client: {e}")))?;

        Ok(Self {
            client,
            base_url,
            user_agent: format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn is_base_url_configured(&self) -> bool {
        !self.base_url.is_empty()
    }

    fn build_headers(
        &self,
        bearer_token: Option<&str>,
        request_id: Option<&str>,
        include_json_content_type: bool,
    ) -> Result<HeaderMap, ServerClientError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&self.user_agent)
                .map_err(|e| ServerClientError::Http(format!("user-agent header: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static(CLIENT_VERSION_HEADER),
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        );
        if include_json_content_type {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }

        if let Some(id) = request_id {
            headers.insert(
                HeaderName::from_static(REQUEST_ID_HEADER),
                HeaderValue::from_str(id)
                    .map_err(|e| ServerClientError::Http(format!("request-id header: {e}")))?,
            );
        }

        if let Some(token) = bearer_token {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|e| ServerClientError::Http(format!("authorization header: {e}")))?,
            );
            headers.insert(
                HeaderName::from_static(LEGACY_TOKEN_HEADER),
                HeaderValue::from_str(token)
                    .map_err(|e| ServerClientError::Http(format!("token header: {e}")))?,
            );
        }

        Ok(headers)
    }

    fn resolve_url(&self, path: &str) -> String {
        let path = path.trim();
        if path.starts_with("http://") || path.starts_with("https://") {
            return path.to_string();
        }
        let path = path.strip_prefix('/').unwrap_or(path);
        format!("{}/{}", self.base_url, path)
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        session: Option<&ServerSession>,
        body: Option<serde_json::Value>,
    ) -> Result<Response, ServerClientError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let url = self.resolve_url(path);
        let bearer = match session {
            Some(s) => s.access_token().await?,
            None => None,
        };
        let headers = self.build_headers(bearer.as_deref(), Some(&request_id), body.is_some())?;

        debug!(%method, %url, request_id = %request_id, "server http request");

        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut builder = self
                .client
                .request(method.clone(), &url)
                .headers(headers.clone());
            if let Some(ref json) = body {
                builder = builder.json(json);
            }

            match builder.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if (status.as_u16() == 429 || status.as_u16() == 503) && attempt < 3 {
                        let delay = Duration::from_millis(250 * 2u64.pow(attempt - 1));
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(err) if err.is_timeout() || err.is_connect() || err.is_request() => {
                    if attempt < 3 {
                        let delay = Duration::from_millis(250 * 2u64.pow(attempt - 1));
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(ServerClientError::Http(err.to_string()));
                }
                Err(err) => return Err(ServerClientError::Http(err.to_string())),
            }
        }
    }

    pub async fn get(
        &self,
        path: &str,
        session: Option<&ServerSession>,
    ) -> Result<Response, ServerClientError> {
        self.request(Method::GET, path, session, None).await
    }

    pub async fn post_json(
        &self,
        path: &str,
        session: Option<&ServerSession>,
        body: serde_json::Value,
    ) -> Result<Response, ServerClientError> {
        self.request(Method::POST, path, session, Some(body)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_url_joins_base_and_path() {
        let transport = HttpTransport {
            client: Client::new(),
            base_url: "https://server.flowyaipc.cn/claw".to_string(),
            user_agent: "test".to_string(),
        };
        assert_eq!(
            transport.resolve_url("/user/me"),
            "https://server.flowyaipc.cn/claw/user/me"
        );
    }
}
