//! Unified auth module: PKCE helpers, token store, and refresh manager.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use url::{form_urlencoded, Url};

/// The only on-disk credential-store envelope version accepted by this crate.
pub const TOKEN_STORE_ENVELOPE_VERSION: u8 = 1;

const TOKEN_STORE_KEY_BYTES: usize = 32;
const TOKEN_STORE_NONCE_BYTES: usize = 12;
const TOKEN_STORE_KEY_ENV: &str = "HERMES_TOKEN_STORE_KEY_B64";

type TokenCache = HashMap<String, OAuthCredential>;

/// A provider credential. Its `Debug` implementation intentionally never emits
/// either token value.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthCredential {
    pub provider: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub scope: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for OAuthCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthCredential")
            .field("provider", &self.provider)
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl OAuthCredential {
    pub fn is_expired(&self, leeway_secs: i64) -> bool {
        match self.expires_at {
            Some(exp) => Utc::now() + Duration::seconds(leeway_secs) >= exp,
            None => false,
        }
    }
}

/// Controls whether opening the token store may create a key or migrate a
/// legacy plaintext file. Runtime resolution must use `ReadOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStoreOpenMode {
    ReadOnly,
    ReadWrite,
}

/// The authenticated state observed when a token store was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStoreState {
    Missing,
    EncryptedV1,
    LegacyPlaintextMigrated,
}

/// A non-secret, typed description of why a credential store could not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStoreError {
    Io { operation: &'static str },
    InvalidKey,
    KeyUnavailable,
    EncryptedStoreMalformed,
    UnsupportedEnvelopeVersion { version: u8 },
    DecryptionFailed,
    LegacyStoreMalformed,
    LegacyMigrationRequired,
    InvalidCredential,
    ReadOnly,
    Serialization,
}

impl CredentialStoreError {
    /// An operator-safe remediation string which does not disclose credential
    /// data or parsed input.
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::LegacyMigrationRequired => {
                "run `hermes auth status` or `hermes auth login <provider>` to migrate it"
            }
            Self::ReadOnly => "use an explicit credential-management command",
            Self::KeyUnavailable
            | Self::InvalidKey
            | Self::DecryptionFailed
            | Self::EncryptedStoreMalformed
            | Self::UnsupportedEnvelopeVersion { .. } => {
                "restore the correct credential-store key or run `hermes auth login <provider>`"
            }
            Self::LegacyStoreMalformed
            | Self::InvalidCredential
            | Self::Serialization
            | Self::Io { .. } => "repair the credential store, then run `hermes auth login <provider>`",
        }
    }
}

impl fmt::Display for CredentialStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation } => write!(formatter, "credential store I/O failed while {operation}"),
            Self::InvalidKey => formatter.write_str("credential store key is invalid"),
            Self::KeyUnavailable => formatter.write_str("credential store key is unavailable"),
            Self::EncryptedStoreMalformed => {
                formatter.write_str("credential store encrypted envelope is malformed")
            }
            Self::UnsupportedEnvelopeVersion { version } => {
                write!(formatter, "credential store envelope version {version} is unsupported")
            }
            Self::DecryptionFailed => formatter.write_str("credential store could not be decrypted"),
            Self::LegacyStoreMalformed => {
                formatter.write_str("credential store legacy plaintext format is malformed")
            }
            Self::LegacyMigrationRequired => {
                formatter.write_str("credential store legacy plaintext migration is required")
            }
            Self::InvalidCredential => formatter.write_str("credential store contains an invalid credential"),
            Self::ReadOnly => formatter.write_str("credential store was opened read-only"),
            Self::Serialization => formatter.write_str("credential store serialization failed"),
        }
    }
}

impl std::error::Error for CredentialStoreError {}

impl From<CredentialStoreError> for AgentError {
    fn from(error: CredentialStoreError) -> Self {
        AgentError::AuthFailed(format!(
            "credential unavailable: {error}; {}",
            error.remediation()
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenStoreEnvelope {
    version: u8,
    nonce_b64: String,
    ciphertext_b64: String,
}

#[derive(Clone)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

impl fmt::Debug for PkcePair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PkcePair")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

/// Number of CSPRNG bytes used by OAuth state and OIDC nonce values.
pub const OAUTH_NONCE_BYTES: usize = 32;

/// Generates an OAuth state or OIDC nonce from 256 bits of CSPRNG output.
///
/// The URL-safe, unpadded encoding is safe to send as an authorization-query
/// parameter and contains no timestamp, process, or host-derived data.
pub fn generate_oauth_nonce() -> String {
    let mut bytes = [0_u8; OAUTH_NONCE_BYTES];
    rand::fill(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

pub fn generate_pkce_pair() -> PkcePair {
    let verifier = generate_oauth_nonce();
    let challenge = pkce_s256_challenge(&verifier);
    PkcePair {
        verifier,
        challenge,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStatus {
    Missing,
    Current { expires_at: Option<DateTime<Utc>> },
    Expired { expires_at: DateTime<Utc> },
    Unusable,
}

#[derive(Clone)]
pub struct FileTokenStore {
    path: PathBuf,
    key: Option<Arc<[u8; TOKEN_STORE_KEY_BYTES]>>,
    cache: Arc<RwLock<TokenCache>>,
    mode: TokenStoreOpenMode,
    state: TokenStoreState,
    write_gate: Arc<Mutex<()>>,
}

impl FileTokenStore {
    /// Opens a writable store for an explicit credential-management operation.
    /// A valid legacy plaintext store is migrated before this returns.
    pub async fn new(path: impl AsRef<Path>) -> Result<Self, CredentialStoreError> {
        Self::open(path, TokenStoreOpenMode::ReadWrite)
    }

    /// Opens a store without creating keys, migrating plaintext, or writing
    /// credentials. Runtime resolution must use this mode.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, CredentialStoreError> {
        Self::open(path, TokenStoreOpenMode::ReadOnly)
    }

    /// Opens a versioned credential store in the requested mode.
    pub fn open(
        path: impl AsRef<Path>,
        mode: TokenStoreOpenMode,
    ) -> Result<Self, CredentialStoreError> {
        let path = path.as_ref().to_path_buf();
        let key_path = path.with_extension("key");
        let raw = match fs::read(&path) {
            Ok(raw) => Some(raw),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(CredentialStoreError::Io { operation: "reading" }),
        };

        let (cache, key, state, migrate_legacy) = match raw {
            None => {
                let key = match mode {
                    TokenStoreOpenMode::ReadOnly => None,
                    TokenStoreOpenMode::ReadWrite => {
                        Some(Arc::new(load_store_key(&key_path, true)?))
                    }
                };
                (HashMap::new(), key, TokenStoreState::Missing, false)
            }
            Some(raw) => {
                let parsed = serde_json::from_slice::<serde_json::Value>(&raw)
                    .map_err(|_| CredentialStoreError::LegacyStoreMalformed)?;
                let object = parsed
                    .as_object()
                    .ok_or(CredentialStoreError::LegacyStoreMalformed)?;
                // A valid legacy cache is a map whose values are credential
                // objects. Encrypted envelopes contain scalar metadata. This
                // avoids misclassifying a legacy provider named `version`,
                // `nonce_b64`, or `ciphertext_b64` as an encrypted file.
                let looks_legacy = object.values().all(serde_json::Value::is_object);

                if !looks_legacy {
                    let envelope = serde_json::from_value::<TokenStoreEnvelope>(parsed)
                        .map_err(|_| CredentialStoreError::EncryptedStoreMalformed)?;
                    let key = Arc::new(load_store_key(&key_path, false)?);
                    let cache = decrypt_token_cache(&key, &envelope)?;
                    validate_token_cache(&cache)
                        .map_err(|_| CredentialStoreError::EncryptedStoreMalformed)?;
                    (
                        cache,
                        Some(key),
                        TokenStoreState::EncryptedV1,
                        false,
                    )
                } else {
                    let cache = serde_json::from_value::<TokenCache>(parsed)
                        .map_err(|_| CredentialStoreError::LegacyStoreMalformed)?;
                    validate_token_cache(&cache)
                        .map_err(|_| CredentialStoreError::LegacyStoreMalformed)?;
                    if mode == TokenStoreOpenMode::ReadOnly {
                        return Err(CredentialStoreError::LegacyMigrationRequired);
                    }
                    (
                        cache,
                        Some(Arc::new(load_store_key(&key_path, true)?)),
                        TokenStoreState::LegacyPlaintextMigrated,
                        true,
                    )
                }
            }
        };

        let store = Self {
            path,
            key,
            cache: Arc::new(RwLock::new(cache)),
            mode,
            state,
            write_gate: Arc::new(Mutex::new(())),
        };
        if migrate_legacy {
            store.flush()?;
        }
        Ok(store)
    }

    pub fn state(&self) -> TokenStoreState {
        self.state
    }

    pub fn get_sync(&self, provider: &str) -> Option<OAuthCredential> {
        self.cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(provider)
            .cloned()
    }

    pub async fn get(&self, provider: &str) -> Option<OAuthCredential> {
        self.get_sync(provider)
    }

    pub fn credential_status(&self, provider: &str, leeway_secs: i64) -> CredentialStatus {
        match self.get_sync(provider) {
            None => CredentialStatus::Missing,
            Some(credential) if credential.access_token.trim().is_empty() => CredentialStatus::Unusable,
            Some(credential) if credential.is_expired(leeway_secs) => {
                CredentialStatus::Expired {
                    expires_at: credential
                        .expires_at
                        .expect("expired credential always has an expiration"),
                }
            }
            Some(credential) => CredentialStatus::Current {
                expires_at: credential.expires_at,
            },
        }
    }

    pub async fn upsert(&self, credential: OAuthCredential) -> Result<(), CredentialStoreError> {
        validate_credential(&credential.provider, &credential)
            .map_err(|_| CredentialStoreError::InvalidCredential)?;
        let _write = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.writable_key()?;
        let provider = credential.provider.clone();
        let mut cache = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = cache.insert(provider.clone(), credential);
        if let Err(error) = self.persist_cache(&cache) {
            if let Some(previous) = previous {
                cache.insert(provider, previous);
            } else {
                cache.remove(&provider);
            }
            return Err(error);
        }
        Ok(())
    }

    pub async fn remove(&self, provider: &str) -> Result<(), CredentialStoreError> {
        let _write = self
            .write_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.writable_key()?;
        let mut cache = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let removed = cache.remove(provider);
        if let Err(error) = self.persist_cache(&cache) {
            if let Some(removed) = removed {
                cache.insert(provider.to_string(), removed);
            }
            return Err(error);
        }
        Ok(())
    }

    pub async fn list_providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = self
            .cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect();
        providers.sort();
        providers
    }

    fn flush(&self) -> Result<(), CredentialStoreError> {
        let cache = self
            .cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.persist_cache(&cache)
    }

    fn writable_key(&self) -> Result<&[u8; TOKEN_STORE_KEY_BYTES], CredentialStoreError> {
        if self.mode != TokenStoreOpenMode::ReadWrite {
            return Err(CredentialStoreError::ReadOnly);
        }
        self.key.as_deref().ok_or(CredentialStoreError::ReadOnly)
    }

    fn persist_cache(&self, cache: &TokenCache) -> Result<(), CredentialStoreError> {
        let key = self.writable_key()?;
        let envelope = encrypt_token_cache(key, cache)?;
        let content =
            serde_json::to_vec_pretty(&envelope).map_err(|_| CredentialStoreError::Serialization)?;
        write_file_private(&self.path, &content)
    }
}

fn validate_token_cache(cache: &TokenCache) -> Result<(), ()> {
    cache
        .iter()
        .try_for_each(|(provider, credential)| validate_credential(provider, credential))
}

fn validate_credential(provider: &str, credential: &OAuthCredential) -> Result<(), ()> {
    if provider.trim().is_empty()
        || credential.provider.trim().is_empty()
        || credential.provider != provider
        || credential.access_token.trim().is_empty()
        || credential.token_type.trim().is_empty()
    {
        return Err(());
    }
    Ok(())
}

fn encrypt_token_cache(
    key: &[u8; TOKEN_STORE_KEY_BYTES],
    cache: &TokenCache,
) -> Result<TokenStoreEnvelope, CredentialStoreError> {
    let plaintext =
        serde_json::to_vec(cache).map_err(|_| CredentialStoreError::Serialization)?;
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| CredentialStoreError::InvalidKey)?;
    let mut nonce_bytes = [0u8; TOKEN_STORE_NONCE_BYTES];
    rand::fill(&mut nonce_bytes[..]);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|_| CredentialStoreError::Serialization)?;
    Ok(TokenStoreEnvelope {
        version: TOKEN_STORE_ENVELOPE_VERSION,
        nonce_b64: base64::engine::general_purpose::STANDARD.encode(nonce_bytes),
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(ciphertext),
    })
}

fn decrypt_token_cache(
    key: &[u8; TOKEN_STORE_KEY_BYTES],
    envelope: &TokenStoreEnvelope,
) -> Result<TokenCache, CredentialStoreError> {
    if envelope.version != TOKEN_STORE_ENVELOPE_VERSION {
        return Err(CredentialStoreError::UnsupportedEnvelopeVersion {
            version: envelope.version,
        });
    }
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(envelope.nonce_b64.trim())
        .map_err(|_| CredentialStoreError::EncryptedStoreMalformed)?;
    if nonce.len() != TOKEN_STORE_NONCE_BYTES {
        return Err(CredentialStoreError::EncryptedStoreMalformed);
    }
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(envelope.ciphertext_b64.trim())
        .map_err(|_| CredentialStoreError::EncryptedStoreMalformed)?;
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| CredentialStoreError::InvalidKey)?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| CredentialStoreError::DecryptionFailed)?;
    serde_json::from_slice::<TokenCache>(&plaintext)
        .map_err(|_| CredentialStoreError::EncryptedStoreMalformed)
}

fn load_store_key(
    path: &Path,
    create_if_missing: bool,
) -> Result<[u8; TOKEN_STORE_KEY_BYTES], CredentialStoreError> {
    if let Ok(raw) = std::env::var(TOKEN_STORE_KEY_ENV) {
        return decode_store_key(raw.trim());
    }
    match fs::read_to_string(path) {
        Ok(raw) => decode_store_key(raw.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create_if_missing => {
            Err(CredentialStoreError::KeyUnavailable)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0u8; TOKEN_STORE_KEY_BYTES];
            rand::fill(&mut key[..]);
            let encoded = base64::engine::general_purpose::STANDARD.encode(key);
            write_file_private(path, encoded.as_bytes())?;
            Ok(key)
        }
        Err(_) => Err(CredentialStoreError::Io {
            operation: "reading the key",
        }),
    }
}

fn decode_store_key(encoded: &str) -> Result<[u8; TOKEN_STORE_KEY_BYTES], CredentialStoreError> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| CredentialStoreError::InvalidKey)?;
    if decoded.len() != TOKEN_STORE_KEY_BYTES {
        return Err(CredentialStoreError::InvalidKey);
    }
    let mut key = [0u8; TOKEN_STORE_KEY_BYTES];
    key.copy_from_slice(&decoded);
    Ok(key)
}

fn write_file_private(path: &Path, content: &[u8]) -> Result<(), CredentialStoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| CredentialStoreError::Io {
            operation: "creating the credential directory",
        })?;
    }
    let tmp_path = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = private_create_new_options()
            .open(&tmp_path)
            .map_err(|_| CredentialStoreError::Io {
                operation: "creating an encrypted credential file",
            })?;
        file.write_all(content)
            .map_err(|_| CredentialStoreError::Io {
                operation: "writing an encrypted credential file",
            })?;
        file.sync_all().map_err(|_| CredentialStoreError::Io {
            operation: "syncing an encrypted credential file",
        })?;
        drop(file);
        set_private_permissions(&tmp_path)?;
        fs::rename(&tmp_path, path).map_err(|_| CredentialStoreError::Io {
            operation: "atomically replacing the credential file",
        })?;
        set_private_permissions(path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn private_create_new_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn set_private_permissions(path: &Path) -> Result<(), CredentialStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| {
            CredentialStoreError::Io {
                operation: "setting credential-file permissions",
            }
        })?;
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

/// The non-secret result of an explicit, expiry-gated refresh attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshResolution {
    NotConfigured,
    Current,
    Refreshed { expires_at: Option<DateTime<Utc>> },
}

#[derive(Clone)]
pub struct AuthManager {
    store: FileTokenStore,
    refresh_handlers: Arc<RwLock<HashMap<String, RefreshHandler>>>,
    refresh_gates: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}

impl AuthManager {
    pub fn new(store: FileTokenStore) -> Self {
        Self {
            store,
            refresh_handlers: Arc::new(RwLock::new(HashMap::new())),
            refresh_gates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register_refresh_handler(&self, provider: &str, handler: RefreshHandler) {
        self.refresh_handlers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(provider.to_string(), handler);
    }

    /// Registers the standard OAuth2 refresh transport for an explicit caller.
    /// Registration does not make refresh automatic.
    pub async fn register_oauth2_refresh_handler(
        &self,
        provider: &str,
        endpoints: OAuth2Endpoints,
    ) {
        let handler: RefreshHandler = Arc::new(move |provider, refresh_token| {
            let endpoints = endpoints.clone();
            Box::pin(async move {
                exchange_refresh_token(&provider, &endpoints, &refresh_token).await
            })
        });
        self.register_refresh_handler(provider, handler).await;
    }

    pub async fn save_credential(&self, credential: OAuthCredential) -> Result<(), AgentError> {
        self.store.upsert(credential).await.map_err(Into::into)
    }

    /// Reads only a usable access token. It never sends refresh traffic or
    /// mutates the store; expired/unusable credentials fail closed.
    pub async fn get_access_token(&self, provider: &str) -> Result<Option<String>, AgentError> {
        match self.store.credential_status(provider, 30) {
            CredentialStatus::Missing => Ok(None),
            CredentialStatus::Current { .. } => Ok(self
                .store
                .get_sync(provider)
                .map(|credential| credential.access_token)),
            CredentialStatus::Expired { .. } => Err(credential_unavailable(
                provider,
                "the stored access token is expired",
            )),
            CredentialStatus::Unusable => Err(credential_unavailable(
                provider,
                "the stored access token is unusable",
            )),
        }
    }

    /// Performs exactly one expiry-gated refresh for this manager/provider.
    /// A handler is required; missing handlers are explicit failures rather
    /// than an implicit `None` result. Successful results are durably written
    /// through the encrypted store before being reported.
    pub async fn refresh_expired_credential(
        &self,
        provider: &str,
    ) -> Result<RefreshResolution, AgentError> {
        let gate = {
            let mut gates = self
                .refresh_gates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            gates
                .entry(provider.to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let _guard = gate.lock().await;

        let Some(current) = self.store.get(provider).await else {
            return Ok(RefreshResolution::NotConfigured);
        };
        if current.access_token.trim().is_empty() {
            return Err(credential_unavailable(
                provider,
                "the stored access token is unusable",
            ));
        }
        if !current.is_expired(30) {
            return Ok(RefreshResolution::Current);
        }
        let refresh_token = current
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                credential_unavailable(provider, "the stored refresh token is unavailable")
            })?
            .to_string();
        let handler = self
            .refresh_handlers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(provider)
            .cloned()
            .ok_or_else(|| {
                AgentError::AuthFailed(format!(
                    "OAuth refresh handler is not configured for provider '{provider}'; run `hermes auth refresh {provider}` after configuring its OAuth endpoints"
                ))
            })?;
        let refreshed = handler(provider.to_string(), refresh_token)
            .await
            .map_err(|_| {
                AgentError::AuthFailed(format!(
                    "OAuth refresh failed for provider '{provider}'; run `hermes auth login {provider}`"
                ))
            })?;
        if refreshed.provider != provider {
            return Err(AgentError::AuthFailed(format!(
                "OAuth refresh returned an unexpected provider for '{provider}'; run `hermes auth login {provider}`"
            )));
        }
        self.store.upsert(refreshed.clone()).await?;
        Ok(RefreshResolution::Refreshed {
            expires_at: refreshed.expires_at,
        })
    }
}

fn credential_unavailable(provider: &str, reason: &str) -> AgentError {
    AgentError::AuthFailed(format!(
        "OAuth credential unavailable for provider '{provider}': {reason}; run `hermes auth refresh {provider}` or `hermes auth login {provider}`"
    ))
}

/// OAuth2 authorization server metadata plus public client id (PKCE public client).
#[derive(Clone)]
pub struct OAuth2Endpoints {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub client_secret: Option<String>,
    pub client_auth_method: OAuth2ClientAuthMethod,
}

impl fmt::Debug for OAuth2Endpoints {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuth2Endpoints")
            .field("authorize_url", &self.authorize_url)
            .field("token_url", &self.token_url)
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("scopes", &self.scopes)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "<redacted>"))
            .field("client_auth_method", &self.client_auth_method)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OAuth2ClientAuthMethod {
    #[default]
    ClientSecretBasic,
    ClientSecretPost,
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

#[derive(Deserialize)]
struct TokenEndpointJson {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    scope: Option<String>,
}

fn oauth_basic_component(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn oauth_client_secret(endpoints: &OAuth2Endpoints) -> Option<&str> {
    endpoints
        .client_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn apply_oauth_client_auth(
    request: reqwest::RequestBuilder,
    endpoints: &OAuth2Endpoints,
    form: &mut Vec<(&'static str, String)>,
) -> reqwest::RequestBuilder {
    let Some(secret) = oauth_client_secret(endpoints) else {
        return request;
    };
    match endpoints.client_auth_method {
        OAuth2ClientAuthMethod::ClientSecretPost => {
            form.push(("client_secret", secret.to_string()));
            request
        }
        OAuth2ClientAuthMethod::ClientSecretBasic => {
            let userpass = format!(
                "{}:{}",
                oauth_basic_component(&endpoints.client_id),
                oauth_basic_component(secret)
            );
            request.header(
                "Authorization",
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(userpass)
                ),
            )
        }
    }
}

/// Exchange `authorization_code` + PKCE verifier at `token_url` (RFC 6749, form body).
pub async fn exchange_authorization_code(
    provider_id: &str,
    endpoints: &OAuth2Endpoints,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthCredential, AgentError> {
    let client = reqwest::Client::new();
    exchange_authorization_code_with_client(&client, provider_id, endpoints, code, code_verifier)
        .await
}

async fn exchange_authorization_code_with_client(
    client: &reqwest::Client,
    provider_id: &str,
    endpoints: &OAuth2Endpoints,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthCredential, AgentError> {
    let mut pairs = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", endpoints.redirect_uri.clone()),
        ("client_id", endpoints.client_id.clone()),
        ("code_verifier", code_verifier.to_string()),
    ];
    let request = client
        .post(&endpoints.token_url)
        .header("Accept", "application/json");
    let request = apply_oauth_client_auth(request, endpoints, &mut pairs);
    let response = request
        .form(&pairs)
        .send()
        .await
        .map_err(|_| AgentError::AuthFailed("OAuth token exchange request failed".into()))?;
    if !response.status().is_success() {
        return Err(AgentError::AuthFailed(format!(
            "OAuth token exchange failed with HTTP {}",
            response.status().as_u16()
        )));
    }
    let body = response
        .json::<TokenEndpointJson>()
        .await
        .map_err(|_| AgentError::AuthFailed("OAuth token exchange response was malformed".into()))?;
    token_endpoint_credential(provider_id, body, None, "exchange")
}

/// Refresh an access token using `refresh_token` (RFC 6749, public client).
pub async fn exchange_refresh_token(
    provider_id: &str,
    endpoints: &OAuth2Endpoints,
    refresh_token: &str,
) -> Result<OAuthCredential, AgentError> {
    let client = reqwest::Client::new();
    exchange_refresh_token_with_client(&client, provider_id, endpoints, refresh_token).await
}

async fn exchange_refresh_token_with_client(
    client: &reqwest::Client,
    provider_id: &str,
    endpoints: &OAuth2Endpoints,
    refresh_token: &str,
) -> Result<OAuthCredential, AgentError> {
    let mut pairs = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", endpoints.client_id.clone()),
    ];
    let request = client
        .post(&endpoints.token_url)
        .header("Accept", "application/json");
    let request = apply_oauth_client_auth(request, endpoints, &mut pairs);
    let response = request
        .form(&pairs)
        .send()
        .await
        .map_err(|_| AgentError::AuthFailed("OAuth token refresh request failed".into()))?;
    if !response.status().is_success() {
        return Err(AgentError::AuthFailed(format!(
            "OAuth token refresh failed with HTTP {}",
            response.status().as_u16()
        )));
    }
    let body = response
        .json::<TokenEndpointJson>()
        .await
        .map_err(|_| AgentError::AuthFailed("OAuth token refresh response was malformed".into()))?;
    token_endpoint_credential(provider_id, body, Some(refresh_token), "refresh")
}

fn token_endpoint_credential(
    provider_id: &str,
    body: TokenEndpointJson,
    fallback_refresh_token: Option<&str>,
    operation: &str,
) -> Result<OAuthCredential, AgentError> {
    if body.access_token.trim().is_empty()
        || body.token_type.as_deref().is_some_and(|token_type| token_type.trim().is_empty())
        || body.expires_in.is_some_and(|seconds| seconds <= 0)
    {
        return Err(AgentError::AuthFailed(format!(
            "OAuth token {operation} response was malformed"
        )));
    }
    let expires_at = body.expires_in.map(|seconds| Utc::now() + Duration::seconds(seconds));
    Ok(OAuthCredential {
        provider: provider_id.to_string(),
        access_token: body.access_token,
        refresh_token: body
            .refresh_token
            .or_else(|| fallback_refresh_token.map(str::to_string)),
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
            client_secret: None,
            client_auth_method: OAuth2ClientAuthMethod::default(),
        };
        let pkce = generate_pkce_pair();
        let url = build_authorization_url(&endpoints, &pkce, "state-xyz").unwrap();
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state-xyz"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("scope=openid"));
    }

    #[test]
    fn oauth_nonce_is_url_safe_256_bit_random_value() {
        let first = generate_oauth_nonce();
        let second = generate_oauth_nonce();

        assert_eq!(first.len(), 43);
        assert_eq!(second.len(), 43);
        assert!(first
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        assert!(second
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
        assert_ne!(first, second);
    }

    #[test]
    fn pkce_s256_challenge_matches_rfc7636_fixture() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn credential_bearing_auth_types_redact_debug_output() {
        let pkce = PkcePair {
            verifier: "pkce-verifier-sentinel".to_string(),
            challenge: "challenge".to_string(),
        };
        let endpoints = OAuth2Endpoints {
            authorize_url: "https://example.com/authorize".to_string(),
            token_url: "https://example.com/token".to_string(),
            client_id: "client".to_string(),
            redirect_uri: "http://127.0.0.1/callback".to_string(),
            scopes: Vec::new(),
            client_secret: Some("client-secret-sentinel".to_string()),
            client_auth_method: OAuth2ClientAuthMethod::ClientSecretBasic,
        };

        assert!(!format!("{pkce:?}").contains("pkce-verifier-sentinel"));
        assert!(!format!("{endpoints:?}").contains("client-secret-sentinel"));
    }

    #[test]
    fn authorization_url_allows_http_redirect_with_arbitrary_host() {
        let endpoints = OAuth2Endpoints {
            authorize_url: "https://example.com/oauth/authorize".to_string(),
            token_url: "https://example.com/oauth/token".to_string(),
            client_id: "cid".to_string(),
            redirect_uri: "http://hermes.internal:9119/auth/callback".to_string(),
            scopes: vec!["openid".to_string()],
            client_secret: None,
            client_auth_method: OAuth2ClientAuthMethod::default(),
        };
        let pkce = generate_pkce_pair();
        let url = build_authorization_url(&endpoints, &pkce, "state-xyz").unwrap();
        assert!(url.contains("redirect_uri=http%3A%2F%2Fhermes.internal%3A9119%2Fauth%2Fcallback"));
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
        assert!(tokio::fs::try_exists(path.with_extension("key"))
            .await
            .unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.with_extension("key"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let reopened = FileTokenStore::new(&path).await.unwrap();
        let got = reopened.get("openai").await.unwrap();
        assert_eq!(got.access_token, "super-secret-token");
        assert!(!format!("{got:?}").contains("super-secret-token"));
    }

    #[cfg(unix)]
    #[test]
    fn private_create_new_options_creates_owner_only_temp_before_write() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json.tmp");

        let mut file = private_create_new_options().open(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        file.write_all(b"secret").unwrap();
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

    #[tokio::test]
    async fn legacy_plaintext_provider_named_envelope_field_migrates_unambiguously() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let mut legacy = HashMap::new();
        legacy.insert(
            "version".to_string(),
            sample_credential("version", "legacy-reserved-provider-token"),
        );
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let store = FileTokenStore::new(&path).await.unwrap();
        assert_eq!(
            store.get("version").await.unwrap().access_token,
            "legacy-reserved-provider-token"
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("legacy-reserved-provider-token"));
        assert_eq!(
            serde_json::from_str::<TokenStoreEnvelope>(&raw)
                .unwrap()
                .version,
            TOKEN_STORE_ENVELOPE_VERSION
        );
    }

    #[tokio::test]
    async fn legacy_plaintext_requires_closed_credential_shape_and_redacts_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let sentinel = "legacy-sentinel-must-not-leak";
        std::fs::write(
            &path,
            format!(
                r#"{{"openai":{{"provider":"openai","access_token":"{sentinel}","token_type":"Bearer","unexpected":"reject"}}}}"#
            ),
        )
        .unwrap();

        let result = FileTokenStore::new(&path).await;
        let error = result.err().expect("invalid legacy store must fail");
        assert_eq!(error, CredentialStoreError::LegacyStoreMalformed);
        assert!(!error.to_string().contains(sentinel));
        assert!(!format!("{error:?}").contains(sentinel));
        assert!(std::fs::read_to_string(&path).unwrap().contains(sentinel));
    }

    #[tokio::test]
    async fn encrypted_store_with_wrong_key_fails_closed_without_token_leakage() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let sentinel = "encrypted-sentinel-must-not-leak";
        let store = FileTokenStore::new(&path).await.unwrap();
        store
            .upsert(sample_credential("openai", sentinel))
            .await
            .unwrap();
        std::fs::write(
            path.with_extension("key"),
            base64::engine::general_purpose::STANDARD.encode([13_u8; TOKEN_STORE_KEY_BYTES]),
        )
        .unwrap();

        let result = FileTokenStore::new(&path).await;
        let error = result.err().expect("wrong key must fail");
        assert_eq!(error, CredentialStoreError::DecryptionFailed);
        assert!(!error.to_string().contains(sentinel));
        assert!(!format!("{error:?}").contains(sentinel));
        assert!(!std::fs::read_to_string(&path).unwrap().contains(sentinel));
    }

    #[tokio::test]
    async fn malformed_encrypted_store_fails_closed_with_typed_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        FileTokenStore::new(&path)
            .await
            .expect("create store key");
        std::fs::write(
            &path,
            br#"{"version":1,"nonce_b64":"AA==","ciphertext_b64":"not-valid-base64"}"#,
        )
        .unwrap();

        let result = FileTokenStore::new(&path).await;
        let error = result
            .err()
            .expect("malformed encrypted store must fail");
        assert_eq!(error, CredentialStoreError::EncryptedStoreMalformed);
    }

    #[tokio::test]
    async fn read_only_open_never_migrates_plaintext_credentials() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let sentinel = "readonly-legacy-sentinel";
        let legacy = serde_json::json!({
            "openai": {
                "provider": "openai",
                "access_token": sentinel,
                "refresh_token": null,
                "token_type": "Bearer",
                "scope": null,
                "expires_at": null
            }
        });
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let error = FileTokenStore::open_read_only(&path)
            .err()
            .expect("read-only legacy load must require explicit migration");
        assert_eq!(error, CredentialStoreError::LegacyMigrationRequired);
        assert!(std::fs::read_to_string(&path).unwrap().contains(sentinel));
        assert!(!path.with_extension("key").exists());
    }

    #[tokio::test]
    async fn read_only_store_rejects_writes_without_mutating_cache_or_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let writable = FileTokenStore::new(&path).await.unwrap();
        writable
            .upsert(sample_credential("openai", "persisted-access-value"))
            .await
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let readonly = FileTokenStore::open_read_only(&path).unwrap();

        assert_eq!(
            readonly.remove("openai").await.unwrap_err(),
            CredentialStoreError::ReadOnly
        );
        assert!(readonly.get("openai").await.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn read_only_missing_store_never_creates_a_key_or_store_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        let store = FileTokenStore::open_read_only(&path).expect("read-only missing store");
        assert_eq!(store.state(), TokenStoreState::Missing);
        assert_eq!(store.credential_status("openai", 0), CredentialStatus::Missing);
        assert!(!path.exists());
        assert!(!path.with_extension("key").exists());
    }

    #[tokio::test]
    async fn passive_access_token_lookup_fails_closed_without_mutating_expired_store() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let access_sentinel = "expired-access-sentinel-must-not-leak";
        let refresh_sentinel = "refresh-sentinel-must-not-leak";
        let store = FileTokenStore::new(&path).await.unwrap();
        store
            .upsert(OAuthCredential {
                provider: "openai".to_string(),
                access_token: access_sentinel.to_string(),
                refresh_token: Some(refresh_sentinel.to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(Utc::now() - Duration::minutes(1)),
            })
            .await
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let manager = AuthManager::new(store);

        let error = manager
            .get_access_token("openai")
            .await
            .expect_err("expired token must be unavailable without an explicit refresh");
        assert!(matches!(error, AgentError::AuthFailed(_)));
        assert!(error.to_string().contains("hermes auth refresh openai"));
        assert!(!error.to_string().contains(access_sentinel));
        assert!(!format!("{error:?}").contains(refresh_sentinel));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn auth_manager_refresh_requires_handler_and_redacts_credential_data() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let sentinel = "expired-access-sentinel";
        let store = FileTokenStore::new(&path).await.unwrap();
        store
            .upsert(OAuthCredential {
                provider: "openai".to_string(),
                access_token: sentinel.to_string(),
                refresh_token: Some("refresh-sentinel".to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(Utc::now() - Duration::minutes(1)),
            })
            .await
            .unwrap();
        let manager = AuthManager::new(store);

        let error = manager
            .refresh_expired_credential("openai")
            .await
            .expect_err("missing handler must be explicit");
        assert!(matches!(error, AgentError::AuthFailed(_)));
        assert!(error.to_string().contains("handler is not configured"));
        assert!(!error.to_string().contains(sentinel));
        assert!(!format!("{error:?}").contains(sentinel));
    }

    #[tokio::test]
    async fn auth_manager_refreshes_expired_credential_through_oauth2_handler() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "refreshed-access-sentinel",
                "refresh_token": "rotated-refresh-sentinel",
                "token_type": "Bearer",
                "expires_in": 120
            })))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = FileTokenStore::new(&path).await.unwrap();
        store
            .upsert(OAuthCredential {
                provider: "openai".to_string(),
                access_token: "expired-access-sentinel".to_string(),
                refresh_token: Some("old-refresh-sentinel".to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(Utc::now() - Duration::minutes(1)),
            })
            .await
            .unwrap();
        let manager = AuthManager::new(store.clone());
        manager
            .register_oauth2_refresh_handler(
                "openai",
                OAuth2Endpoints {
                    authorize_url: "https://unused.example/authorize".to_string(),
                    token_url: format!("{}/token", server.uri()),
                    client_id: "test-client".to_string(),
                    redirect_uri: "http://127.0.0.1/unused".to_string(),
                    scopes: Vec::new(),
                    client_secret: None,
                    client_auth_method: OAuth2ClientAuthMethod::default(),
                },
            )
            .await;

        let result = manager.refresh_expired_credential("openai").await.unwrap();
        assert!(matches!(result, RefreshResolution::Refreshed { .. }));
        assert_eq!(
            store.get("openai").await.unwrap().access_token,
            "refreshed-access-sentinel"
        );
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(!raw.contains("refreshed-access-sentinel"));
        assert!(!raw.contains("old-refresh-sentinel"));
    }

    #[tokio::test]
    async fn refresh_timeout_is_typed_and_redacted() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(120))
                    .set_body_json(serde_json::json!({
                        "access_token": "late-response-sentinel",
                        "expires_in": 60
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(20))
            .build()
            .unwrap();
        let endpoints = OAuth2Endpoints {
            authorize_url: "https://unused.example/authorize".to_string(),
            token_url: format!("{}/token", server.uri()),
            client_id: "test-client".to_string(),
            redirect_uri: "http://127.0.0.1/unused".to_string(),
            scopes: Vec::new(),
            client_secret: None,
            client_auth_method: OAuth2ClientAuthMethod::default(),
        };

        let error = exchange_refresh_token_with_client(
            &client,
            "openai",
            &endpoints,
            "timeout-refresh-sentinel",
        )
        .await
        .expect_err("timeout must fail");
        assert!(matches!(error, AgentError::AuthFailed(_)));
        assert!(error.to_string().contains("request failed"));
        assert!(!error.to_string().contains("timeout-refresh-sentinel"));
        assert!(!format!("{error:?}").contains("late-response-sentinel"));
    }

    #[tokio::test]
    async fn file_token_store_lists_providers_sorted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = FileTokenStore::new(&path).await.unwrap();

        store
            .upsert(sample_credential("nous", "n-1"))
            .await
            .unwrap();
        store
            .upsert(sample_credential("openai", "o-1"))
            .await
            .unwrap();
        store
            .upsert(sample_credential("anthropic", "a-1"))
            .await
            .unwrap();

        let providers = store.list_providers().await;
        assert_eq!(providers, vec!["anthropic", "nous", "openai"]);
    }
}
