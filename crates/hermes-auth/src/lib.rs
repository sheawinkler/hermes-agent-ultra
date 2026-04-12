//! Unified auth module: PKCE helpers, token store, and refresh manager.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub provider: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub scope: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl OAuthCredential {
    pub fn is_expired(&self, leeway_secs: i64) -> bool {
        match self.expires_at {
            Some(exp) => Utc::now() + Duration::seconds(leeway_secs) >= exp,
            None => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate_pkce_pair() -> PkcePair {
    let verifier = uuid::Uuid::new_v4().to_string().replace('-', "");
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    PkcePair {
        verifier,
        challenge,
    }
}

#[derive(Clone)]
pub struct FileTokenStore {
    path: PathBuf,
    cache: Arc<RwLock<HashMap<String, OAuthCredential>>>,
}

impl FileTokenStore {
    pub async fn new(path: impl AsRef<Path>) -> Result<Self, AgentError> {
        let path = path.as_ref().to_path_buf();
        let initial = if tokio::fs::try_exists(&path)
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?
        {
            let raw = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| AgentError::Io(e.to_string()))?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            HashMap::new()
        };
        Ok(Self {
            path,
            cache: Arc::new(RwLock::new(initial)),
        })
    }

    pub async fn get(&self, provider: &str) -> Option<OAuthCredential> {
        self.cache.read().await.get(provider).cloned()
    }

    pub async fn upsert(&self, credential: OAuthCredential) -> Result<(), AgentError> {
        self.cache
            .write()
            .await
            .insert(credential.provider.clone(), credential);
        self.flush().await
    }

    pub async fn remove(&self, provider: &str) -> Result<(), AgentError> {
        self.cache.write().await.remove(provider);
        self.flush().await
    }

    async fn flush(&self) -> Result<(), AgentError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AgentError::Io(e.to_string()))?;
        }
        let content = serde_json::to_string_pretty(&*self.cache.read().await)
            .map_err(|e| AgentError::Config(e.to_string()))?;
        tokio::fs::write(&self.path, content)
            .await
            .map_err(|e| AgentError::Io(e.to_string()))
    }
}

pub type RefreshHandler = Arc<
    dyn Fn(
            String,
            String,
        ) -> futures::future::BoxFuture<'static, Result<OAuthCredential, AgentError>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct AuthManager {
    store: FileTokenStore,
    refresh_handlers: Arc<RwLock<HashMap<String, RefreshHandler>>>,
}

impl AuthManager {
    pub fn new(store: FileTokenStore) -> Self {
        Self {
            store,
            refresh_handlers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register_refresh_handler(&self, provider: &str, handler: RefreshHandler) {
        self.refresh_handlers
            .write()
            .await
            .insert(provider.to_string(), handler);
    }

    pub async fn save_credential(&self, credential: OAuthCredential) -> Result<(), AgentError> {
        self.store.upsert(credential).await
    }

    pub async fn get_access_token(&self, provider: &str) -> Result<Option<String>, AgentError> {
        let Some(mut credential) = self.store.get(provider).await else {
            return Ok(None);
        };

        if credential.is_expired(30) {
            let Some(refresh_token) = credential.refresh_token.clone() else {
                return Ok(None);
            };
            let handler = self
                .refresh_handlers
                .read()
                .await
                .get(provider)
                .cloned()
                .ok_or_else(|| {
                    AgentError::AuthFailed(format!("no refresh handler for {}", provider))
                })?;
            credential = handler(provider.to_string(), refresh_token).await?;
            self.store.upsert(credential.clone()).await?;
        }

        Ok(Some(credential.access_token))
    }
}

/// OAuth2 authorization server metadata plus public client id (PKCE public client).
#[derive(Debug, Clone)]
pub struct OAuth2Endpoints {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

/// Build the browser `authorization_url` (code challenge S256 + state CSRF token).
pub fn build_authorization_url(
    endpoints: &OAuth2Endpoints,
    pkce: &PkcePair,
    state: &str,
) -> Result<String, AgentError> {
    let mut url = Url::parse(&endpoints.authorize_url)
        .map_err(|e| AgentError::Config(format!("invalid authorize_url: {}", e)))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", &endpoints.client_id);
        q.append_pair("redirect_uri", &endpoints.redirect_uri);
        q.append_pair("code_challenge", &pkce.challenge);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("state", state);
        if !endpoints.scopes.is_empty() {
            q.append_pair("scope", &endpoints.scopes.join(" "));
        }
    }
    Ok(url.into())
}

#[derive(Debug, Deserialize)]
struct TokenEndpointJson {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    scope: Option<String>,
}

/// Exchange `authorization_code` + PKCE verifier at `token_url` (RFC 6749, form body).
pub async fn exchange_authorization_code(
    provider_id: &str,
    endpoints: &OAuth2Endpoints,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthCredential, AgentError> {
    let client = reqwest::Client::new();
    let pairs = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", endpoints.redirect_uri.as_str()),
        ("client_id", endpoints.client_id.as_str()),
        ("code_verifier", code_verifier),
    ];
    let resp = client
        .post(&endpoints.token_url)
        .header("Accept", "application/json")
        .form(&pairs)
        .send()
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(AgentError::AuthFailed(format!(
            "token exchange failed: {}",
            text
        )));
    }

    let body: TokenEndpointJson = resp
        .json()
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;
    let expires_at = body.expires_in.map(|s| Utc::now() + Duration::seconds(s));
    Ok(OAuthCredential {
        provider: provider_id.to_string(),
        access_token: body.access_token,
        refresh_token: body.refresh_token,
        token_type: body.token_type.unwrap_or_else(|| "Bearer".to_string()),
        scope: body.scope,
        expires_at,
    })
}

/// Refresh an access token using `refresh_token` (RFC 6749, public client).
pub async fn exchange_refresh_token(
    provider_id: &str,
    endpoints: &OAuth2Endpoints,
    refresh_token: &str,
) -> Result<OAuthCredential, AgentError> {
    let client = reqwest::Client::new();
    let pairs = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", endpoints.client_id.as_str()),
    ];
    let resp = client
        .post(&endpoints.token_url)
        .header("Accept", "application/json")
        .form(&pairs)
        .send()
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(AgentError::AuthFailed(format!(
            "token refresh failed: {}",
            text
        )));
    }

    let body: TokenEndpointJson = resp
        .json()
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;
    let expires_at = body.expires_in.map(|s| Utc::now() + Duration::seconds(s));
    Ok(OAuthCredential {
        provider: provider_id.to_string(),
        access_token: body.access_token,
        refresh_token: body
            .refresh_token
            .or_else(|| Some(refresh_token.to_string())),
        token_type: body.token_type.unwrap_or_else(|| "Bearer".to_string()),
        scope: body.scope,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_contains_pkce_and_state() {
        let endpoints = OAuth2Endpoints {
            authorize_url: "https://example.com/oauth/authorize".to_string(),
            token_url: "https://example.com/oauth/token".to_string(),
            client_id: "cid".to_string(),
            redirect_uri: "http://127.0.0.1/cb".to_string(),
            scopes: vec!["openid".to_string()],
        };
        let pkce = generate_pkce_pair();
        let url = build_authorization_url(&endpoints, &pkce, "state-xyz").unwrap();
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state-xyz"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("scope=openid"));
    }
}
