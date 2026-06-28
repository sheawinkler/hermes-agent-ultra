#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCatalogEntry {
    pub provider: String,
    pub models: Vec<String>,
    pub total_models: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogCacheStatus {
    pub verified: bool,
    pub age_secs: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProviderCatalogCacheRecord {
    version: u32,
    provider: String,
    generated_at: String,
    models: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProviderCatalogCacheSignature {
    version: u32,
    algorithm: String,
    key_id: String,
    payload_sha256: String,
    signature_hex: String,
    signed_at: String,
}

fn catalog_cache_ttl_secs() -> i64 {
    std::env::var("HERMES_PROVIDER_MODEL_CACHE_TTL_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(30 * 60)
}

fn provider_catalog_cache_dir() -> PathBuf {
    hermes_config::hermes_home()
        .join("cache")
        .join("provider-model-catalog")
}

fn provider_catalog_cache_path(provider: &str) -> PathBuf {
    provider_catalog_cache_dir().join(format!("{}.json", provider.trim().to_ascii_lowercase()))
}

fn provider_catalog_signature_path(provider: &str) -> PathBuf {
    provider_catalog_cache_dir().join(format!("{}.sig.json", provider.trim().to_ascii_lowercase()))
}

fn parse_hex_key(raw: &str) -> Option<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let decoded = hex::decode(trimmed).ok()?;
    if decoded.len() < 16 {
        return None;
    }
    Some(decoded)
}

fn ensure_provenance_key() -> Option<Vec<u8>> {
    if let Ok(raw) = std::env::var("HERMES_PROVENANCE_SIGNING_KEY") {
        if let Some(key) = parse_hex_key(&raw) {
            return Some(key);
        }
    }
    let key_path = hermes_config::hermes_home()
        .join("auth")
        .join("provenance.key");
    if let Ok(raw) = std::fs::read_to_string(&key_path) {
        if let Some(key) = parse_hex_key(&raw) {
            return Some(key);
        }
    }

    let parent = key_path.parent()?;
    if std::fs::create_dir_all(parent).is_err() {
        return None;
    }
    let mut key = [0u8; 32];
    rand::fill(&mut key[..]);
    let encoded = hex::encode(key);
    if std::fs::write(&key_path, format!("{encoded}\n")).is_err() {
        return None;
    }
    Some(key.to_vec())
}

fn cache_key_id(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    let hexed = hex::encode(digest);
    format!("k-{}", &hexed[..16])
}

fn sign_cache_payload(bytes: &[u8]) -> Option<ProviderCatalogCacheSignature> {
    let key = ensure_provenance_key()?;
    let payload_sha = hex::encode(Sha256::digest(bytes));
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(payload_sha.as_bytes());
    let signature_hex = hex::encode(mac.finalize().into_bytes());
    Some(ProviderCatalogCacheSignature {
        version: PROVIDER_CATALOG_CACHE_VERSION,
        algorithm: "hmac-sha256".to_string(),
        key_id: cache_key_id(&key),
        payload_sha256: payload_sha,
        signature_hex,
        signed_at: Utc::now().to_rfc3339(),
    })
}

fn verify_cache_payload(bytes: &[u8], signature: &ProviderCatalogCacheSignature) -> Option<bool> {
    let key = ensure_provenance_key()?;
    let payload_sha = hex::encode(Sha256::digest(bytes));
    if payload_sha != signature.payload_sha256 {
        return Some(false);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(payload_sha.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    Some(expected == signature.signature_hex)
}

fn provider_catalog_cache_status(provider: &str) -> Option<CatalogCacheStatus> {
    let path = provider_catalog_cache_path(provider);
    let sig_path = provider_catalog_signature_path(provider);
    let payload_bytes = std::fs::read(path).ok()?;
    let payload: ProviderCatalogCacheRecord = serde_json::from_slice(&payload_bytes).ok()?;
    let sig_raw = std::fs::read_to_string(sig_path).ok()?;
    let signature: ProviderCatalogCacheSignature = serde_json::from_str(&sig_raw).ok()?;
    let verified = verify_cache_payload(&payload_bytes, &signature).unwrap_or(false);
    let age_secs = DateTime::parse_from_rfc3339(&payload.generated_at)
        .ok()
        .map(|ts| Utc::now().signed_duration_since(ts.with_timezone(&Utc)))
        .and_then(|delta| u64::try_from(delta.num_seconds().max(0)).ok());
    Some(CatalogCacheStatus { verified, age_secs })
}

pub fn cached_provider_catalog_status(provider: &str) -> Option<CatalogCacheStatus> {
    provider_catalog_cache_status(provider)
}

pub fn clear_provider_catalog_cache(provider: &str) -> Result<bool, AgentError> {
    let mut removed = false;
    for path in [
        provider_catalog_cache_path(provider),
        provider_catalog_signature_path(provider),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => removed = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(AgentError::Io(format!(
                    "Failed to clear provider catalog cache {}: {}",
                    path.display(),
                    err
                )));
            }
        }
    }
    Ok(removed)
}

fn load_provider_catalog_cache(provider: &str) -> Option<Vec<String>> {
    let ttl = catalog_cache_ttl_secs();
    let status = provider_catalog_cache_status(provider)?;
    if !status.verified {
        return None;
    }
    if let Some(age) = status.age_secs {
        if age > ttl as u64 {
            return None;
        }
    }
    let path = provider_catalog_cache_path(provider);
    let payload_raw = std::fs::read_to_string(path).ok()?;
    let payload: ProviderCatalogCacheRecord = serde_json::from_str(&payload_raw).ok()?;
    if payload.version != PROVIDER_CATALOG_CACHE_VERSION {
        return None;
    }
    let normalized = provider.trim().to_ascii_lowercase();
    if payload.provider.trim().to_ascii_lowercase() != normalized {
        return None;
    }
    Some(payload.models)
}

fn persist_provider_catalog_cache(provider: &str, models: &[String]) {
    let record = ProviderCatalogCacheRecord {
        version: PROVIDER_CATALOG_CACHE_VERSION,
        provider: provider.trim().to_ascii_lowercase(),
        generated_at: Utc::now().to_rfc3339(),
        models: models.to_vec(),
    };
    let Ok(payload_bytes) = serde_json::to_vec_pretty(&record) else {
        return;
    };
    let Some(signature) = sign_cache_payload(&payload_bytes) else {
        return;
    };
    let Ok(sig_bytes) = serde_json::to_vec_pretty(&signature) else {
        return;
    };
    let cache_path = provider_catalog_cache_path(provider);
    let sig_path = provider_catalog_signature_path(provider);
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp_payload = cache_path.with_extension("json.tmp");
    let tmp_sig = sig_path.with_extension("sig.json.tmp");
    if let Ok(mut file) = std::fs::File::create(&tmp_payload) {
        let _ = file.write_all(&payload_bytes);
        let _ = file.flush();
        let _ = std::fs::rename(&tmp_payload, &cache_path);
    }
    if let Ok(mut file) = std::fs::File::create(&tmp_sig) {
        let _ = file.write_all(&sig_bytes);
        let _ = file.flush();
        let _ = std::fs::rename(&tmp_sig, &sig_path);
    }
}
