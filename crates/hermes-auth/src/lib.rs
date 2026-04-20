//! Unified auth module: PKCE helpers, token store, and refresh manager.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hermes_core::AgentError;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use url::Url;

const TOKEN_STORE_ENVELOPE_VERSION: u8 = 1;
const TOKEN_STORE_KEY_BYTES: usize = 32;
const TOKEN_STORE_NONCE_BYTES: usize = 12;
const TOKEN_STORE_KEY_ENV: &str = "HERMES_TOKEN_STORE_KEY_B64";

type TokenCache = HashMap<String, OAuthCredential>;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenStoreEnvelope {
    version: u8,
    nonce_b64: String,
    ciphertext_b64: String,
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
    key: Arc<[u8; TOKEN_STORE_KEY_BYTES]>,
    cache: Arc<RwLock<TokenCache>>,
}

impl FileTokenStore {
    pub async fn new(path: impl AsRef<Path>) -> Result<Self, AgentError> {
        let path = path.as_ref().to_path_buf();
        let key_path = path.with_extension("key");
        let key = load_or_create_store_key(&key_path).await?;
        let (initial, migrate_legacy) = load_token_cache(&path, &key).await?;
        let store = Self {
            path,
            key: Arc::new(key),
            cache: Arc::new(RwLock::new(initial)),
        };
        if migrate_legacy {
            // Rewrite legacy plaintext content to encrypted envelope format.
            store.flush().await?;
        }
        Ok(store)
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
        let content = {
            let cache = self.cache.read().await;
            let envelope = encrypt_token_cache(&self.key, &cache)?;
            serde_json::to_vec_pretty(&envelope).map_err(|e| AgentError::Config(e.to_string()))?
        };
        write_file_private(&self.path, &content).await
    }
}

async fn load_token_cache(
    path: &Path,
    key: &[u8; TOKEN_STORE_KEY_BYTES],
) -> Result<(TokenCache, bool), AgentError> {
    if !tokio::fs::try_exists(path)
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?
    {
        return Ok((HashMap::new(), false));
    }
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;
    if raw.trim().is_empty() {
        return Ok((HashMap::new(), false));
    }

    if let Ok(envelope) = serde_json::from_str::<TokenStoreEnvelope>(&raw) {
        let cache = decrypt_token_cache(key, &envelope)?;
        return Ok((cache, false));
    }

    let legacy: TokenCache =
        serde_json::from_str(&raw).map_err(|e| AgentError::Config(e.to_string()))?;
    tracing::warn!(
        path = %path.display(),
        "Legacy plaintext token store detected; migrating to encrypted format"
    );
    Ok((legacy, true))
}

fn encrypt_token_cache(
    key: &[u8; TOKEN_STORE_KEY_BYTES],
    cache: &TokenCache,
) -> Result<TokenStoreEnvelope, AgentError> {
    let plaintext = serde_json::to_vec(cache).map_err(|e| AgentError::Config(e.to_string()))?;
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| AgentError::Config(e.to_string()))?;
    let mut nonce_bytes = [0u8; TOKEN_STORE_NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|_| AgentError::AuthFailed("failed to encrypt token store".into()))?;
    Ok(TokenStoreEnvelope {
        version: TOKEN_STORE_ENVELOPE_VERSION,
        nonce_b64: base64::engine::general_purpose::STANDARD.encode(nonce_bytes),
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(ciphertext),
    })
}

fn decrypt_token_cache(
    key: &[u8; TOKEN_STORE_KEY_BYTES],
    envelope: &TokenStoreEnvelope,
) -> Result<TokenCache, AgentError> {
    if envelope.version != TOKEN_STORE_ENVELOPE_VERSION {
        return Err(AgentError::Config(format!(
            "unsupported token store envelope version: {}",
            envelope.version
        )));
    }
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(envelope.nonce_b64.trim())
        .map_err(|e| AgentError::Config(format!("invalid token store nonce: {}", e)))?;
    if nonce.len() != TOKEN_STORE_NONCE_BYTES {
        return Err(AgentError::Config(format!(
            "invalid token store nonce length: {}",
            nonce.len()
        )));
    }
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(envelope.ciphertext_b64.trim())
        .map_err(|e| AgentError::Config(format!("invalid token store payload: {}", e)))?;
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| AgentError::Config(e.to_string()))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| AgentError::AuthFailed("failed to decrypt token store".into()))?;
    serde_json::from_slice::<TokenCache>(&plaintext).map_err(|e| AgentError::Config(e.to_string()))
}

async fn load_or_create_store_key(path: &Path) -> Result<[u8; TOKEN_STORE_KEY_BYTES], AgentError> {
    if let Ok(raw) = std::env::var(TOKEN_STORE_KEY_ENV) {
        return decode_store_key(raw.trim());
    }
    if tokio::fs::try_exists(path)
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?
    {
        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;
        return decode_store_key(raw.trim());
    }

    let mut key = [0u8; TOKEN_STORE_KEY_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);
    write_file_private(path, encoded.as_bytes()).await?;
    Ok(key)
}

fn decode_store_key(encoded: &str) -> Result<[u8; TOKEN_STORE_KEY_BYTES], AgentError> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| AgentError::Config(format!("invalid {}: {}", TOKEN_STORE_KEY_ENV, e)))?;
    if decoded.len() != TOKEN_STORE_KEY_BYTES {
        return Err(AgentError::Config(format!(
            "{} must decode to exactly {} bytes (got {})",
            TOKEN_STORE_KEY_ENV,
            TOKEN_STORE_KEY_BYTES,
            decoded.len()
        )));
    }
    let mut key = [0u8; TOKEN_STORE_KEY_BYTES];
    key.copy_from_slice(&decoded);
    Ok(key)
}

async fn write_file_private(path: &Path, content: &[u8]) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;
    }
    let tmp_path = path.with_extension(format!(
        "{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    tokio::fs::write(&tmp_path, content)
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;
    set_private_permissions(&tmp_path).await?;
    tokio::fs::rename(&tmp_path, path)
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;
    set_private_permissions(path).await
}

async fn set_private_permissions(path: &Path) -> Result<(), AgentError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(path, perms)
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
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
    use tempfile::tempdir;

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

    fn sample_credential(provider: &str, access_token: &str) -> OAuthCredential {
        OAuthCredential {
            provider: provider.to_string(),
            access_token: access_token.to_string(),
            refresh_token: Some("refresh-token".to_string()),
            token_type: "Bearer".to_string(),
            scope: Some("openid".to_string()),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn file_token_store_encrypts_and_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        let store = FileTokenStore::new(&path).await.unwrap();
        store
            .upsert(sample_credential("openai", "super-secret-token"))
            .await
            .unwrap();

        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(!raw.contains("super-secret-token"));
        let envelope: TokenStoreEnvelope = serde_json::from_str(&raw).unwrap();
        assert_eq!(envelope.version, TOKEN_STORE_ENVELOPE_VERSION);
        assert!(tokio::fs::try_exists(path.with_extension("key")).await.unwrap());

        let reopened = FileTokenStore::new(&path).await.unwrap();
        let got = reopened.get("openai").await.unwrap();
        assert_eq!(got.access_token, "super-secret-token");
    }

    #[tokio::test]
    async fn file_token_store_migrates_legacy_plaintext() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        let mut legacy = HashMap::new();
        legacy.insert(
            "anthropic".to_string(),
            sample_credential("anthropic", "legacy-token"),
        );
        let legacy_json = serde_json::to_string_pretty(&legacy).unwrap();
        tokio::fs::write(&path, legacy_json).await.unwrap();

        let store = FileTokenStore::new(&path).await.unwrap();
        let got = store.get("anthropic").await.unwrap();
        assert_eq!(got.access_token, "legacy-token");

        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(!raw.contains("legacy-token"));
        let envelope: TokenStoreEnvelope = serde_json::from_str(&raw).unwrap();
        assert_eq!(envelope.version, TOKEN_STORE_ENVELOPE_VERSION);
    }
}
