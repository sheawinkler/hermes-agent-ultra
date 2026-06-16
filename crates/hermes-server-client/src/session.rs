//! Server session token storage (encrypted file store + env override).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use hermes_auth::{FileTokenStore, OAuthCredential};
use hermes_config::ServerConfig;
use tracing::debug;

use crate::error::ServerClientError;

/// Token store provider key for the remote LLM server.
pub const SERVER_TOKEN_PROVIDER: &str = "server";

const SERVER_TOKEN_ENV: &str = "HERMES_SERVER_TOKEN";

/// Issued credentials for LLM server API calls.
#[derive(Debug, Clone)]
pub struct ServerTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub token_type: String,
}

impl ServerTokens {
    pub fn from_credential(credential: OAuthCredential) -> Self {
        Self {
            access_token: credential.access_token,
            refresh_token: credential.refresh_token,
            expires_at: credential.expires_at,
            token_type: credential.token_type,
        }
    }

    pub fn from_jwt(access_token: String) -> Self {
        Self {
            access_token,
            refresh_token: None,
            expires_at: None,
            token_type: "Bearer".to_string(),
        }
    }

    pub fn into_credential(self) -> OAuthCredential {
        OAuthCredential {
            provider: SERVER_TOKEN_PROVIDER.to_string(),
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            token_type: self.token_type,
            scope: None,
            expires_at: self.expires_at,
        }
    }

    pub fn is_expired(&self, leeway_secs: i64) -> bool {
        match self.expires_at {
            Some(exp) => Utc::now() + chrono::Duration::seconds(leeway_secs) >= exp,
            None => false,
        }
    }
}

/// Login/session state for the remote LLM server.
#[derive(Debug, Clone)]
pub struct ServerSession {
    token_store_path: PathBuf,
    env_token: Option<String>,
}

impl ServerSession {
    pub fn from_config(config: &ServerConfig, hermes_home: impl AsRef<Path>) -> Self {
        let _ = config;
        let env_token = std::env::var(SERVER_TOKEN_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        Self {
            token_store_path: hermes_home.as_ref().join("auth").join("tokens.json"),
            env_token,
        }
    }

    pub async fn token_source(&self) -> TokenSource {
        if self.env_token.is_some() {
            return TokenSource::Environment;
        }
        match self.load_tokens().await {
            Ok(Some(_)) => TokenSource::FileStore,
            _ => TokenSource::None,
        }
    }

    async fn token_store(&self) -> Result<FileTokenStore, ServerClientError> {
        if let Some(parent) = self.token_store_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ServerClientError::Agent(hermes_core::AgentError::Io(e.to_string()))
            })?;
        }
        FileTokenStore::new(&self.token_store_path)
            .await
            .map_err(ServerClientError::Agent)
    }

    pub fn effective_access_token(&self) -> Option<String> {
        self.env_token.clone()
    }

    pub async fn access_token(&self) -> Result<Option<String>, ServerClientError> {
        Ok(self.load_tokens().await?.map(|t| t.access_token))
    }

    pub async fn load_tokens(&self) -> Result<Option<ServerTokens>, ServerClientError> {
        if let Some(token) = self.env_token.clone() {
            return Ok(Some(ServerTokens {
                access_token: token,
                refresh_token: None,
                expires_at: None,
                token_type: "Bearer".to_string(),
            }));
        }

        let store = self.token_store().await?;
        Ok(store
            .get(SERVER_TOKEN_PROVIDER)
            .await
            .map(ServerTokens::from_credential))
    }

    pub async fn save_tokens(&self, tokens: ServerTokens) -> Result<(), ServerClientError> {
        if self.env_token.is_some() {
            debug!("HERMES_SERVER_TOKEN set; skipping file token store write");
            return Ok(());
        }
        let store = self.token_store().await?;
        store
            .upsert(tokens.into_credential())
            .await
            .map_err(ServerClientError::Agent)
    }

    pub async fn logout(&self) -> Result<bool, ServerClientError> {
        if self.env_token.is_some() {
            return Ok(false);
        }
        let store = self.token_store().await?;
        let had = store.get(SERVER_TOKEN_PROVIDER).await.is_some();
        if had {
            store
                .remove(SERVER_TOKEN_PROVIDER)
                .await
                .map_err(ServerClientError::Agent)?;
        }
        Ok(had)
    }

    pub async fn refresh_if_needed(&self) -> Result<Option<ServerTokens>, ServerClientError> {
        let Some(tokens) = self.load_tokens().await? else {
            return Ok(None);
        };
        if !tokens.is_expired(30) {
            return Ok(Some(tokens));
        }
        // Refresh endpoint wiring deferred until server auth docs arrive.
        Err(ServerClientError::not_configured("token refresh"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    Environment,
    FileStore,
    None,
}

impl std::fmt::Display for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Environment => write!(f, "environment (HERMES_SERVER_TOKEN)"),
            Self::FileStore => write!(f, "local encrypted token store"),
            Self::None => write!(f, "not logged in"),
        }
    }
}
