//! Skills Hub client for interacting with the agentskills.io API.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use tracing::{debug, instrument};

use hermes_core::types::{Skill, SkillMeta};

use crate::skill::SkillError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default base URL for the Skills Hub API.
pub const DEFAULT_HUB_URL: &str = "https://agentskills.io/api/v1";
const HUB_TRUSTED_KEYS_ENV: &str = "HERMES_SKILLS_HUB_TRUSTED_KEYS";

// ---------------------------------------------------------------------------
// Hub API request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct SearchRequest {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    skills: Vec<SkillMeta>,
}

#[derive(Debug, Deserialize)]
struct DownloadResponse {
    skill: Skill,
    #[serde(default)]
    source_signature: Option<String>,
}

/// Request body for the batch version check endpoint.
#[derive(Debug, Serialize)]
struct CheckUpdatesRequest {
    /// Names (or IDs) of skills to check.
    skills: Vec<SkillVersionEntry>,
}

/// A single skill entry for the version check request.
#[derive(Debug, Serialize)]
struct SkillVersionEntry {
    name: String,
    /// Current local version hash.
    version: String,
}

/// Response from the batch version check endpoint.
#[derive(Debug, Deserialize)]
struct CheckUpdatesResponse {
    updates: Vec<SkillUpdate>,
}

/// Information about an available update for a single skill.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillUpdate {
    /// Skill name.
    pub name: String,
    /// Version currently installed locally.
    pub current_version: String,
    /// Latest version available on the hub.
    pub latest_version: String,
    /// Short changelog or summary (if provided by the hub).
    #[serde(default)]
    pub changelog: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrySkillMeta {
    pub name: String,
    pub description: String,
    pub source: String,
    pub identifier: String,
    pub trust_level: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClawHubBundle {
    pub name: String,
    pub version: Option<String>,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClawHubFileRef {
    pub path: String,
    pub content: Option<String>,
    pub raw_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct UploadRequest {
    name: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    id: String,
}

fn value_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn clawhub_tags(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(ToString::to_string)
            .collect(),
        Value::Object(map) => {
            let mut tags: Vec<String> = map
                .keys()
                .filter(|key| key.as_str() != "latest")
                .cloned()
                .collect();
            tags.sort();
            tags
        }
        _ => Vec::new(),
    }
}

pub fn clawhub_meta_from_payload(
    payload: &Value,
    fallback_slug: &str,
) -> Option<RegistrySkillMeta> {
    let skill = payload.get("skill").unwrap_or(payload);
    let identifier = value_str(skill, &["slug", "id", "identifier"])
        .unwrap_or(fallback_slug)
        .trim();
    if identifier.is_empty() {
        return None;
    }
    let name = value_str(skill, &["displayName", "display_name", "name", "slug"])
        .unwrap_or(identifier)
        .trim()
        .to_string();
    let description = value_str(skill, &["summary", "description"])
        .unwrap_or("")
        .trim()
        .to_string();
    let version = payload
        .get("latestVersion")
        .and_then(|v| value_str(v, &["version"]))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    Some(RegistrySkillMeta {
        name,
        description,
        source: "clawhub".to_string(),
        identifier: identifier.to_string(),
        trust_level: "community".to_string(),
        tags: skill.get("tags").map(clawhub_tags).unwrap_or_default(),
        version,
    })
}

pub fn clawhub_metas_from_listing(payload: &Value) -> Vec<RegistrySkillMeta> {
    payload
        .get("items")
        .or_else(|| payload.get("skills"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let fallback = value_str(item, &["slug", "id", "identifier"]).unwrap_or("");
            clawhub_meta_from_payload(item, fallback)
        })
        .collect()
}

fn normalize_search_key(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn clawhub_score(meta: &RegistrySkillMeta, query: &str) -> i32 {
    let q = normalize_search_key(query);
    if q.is_empty() {
        return 1;
    }
    let id = normalize_search_key(&meta.identifier);
    let name = normalize_search_key(&meta.name);
    let desc = meta.description.to_ascii_lowercase();
    if id == q || name == q {
        1000
    } else if id.starts_with(&q) || name.starts_with(&q) {
        800
    } else if id.contains(&q) || name.contains(&q) {
        650
    } else if desc.contains(&query.to_ascii_lowercase()) {
        250
    } else {
        0
    }
}

pub fn clawhub_finalize_search_results(
    query: &str,
    mut candidates: Vec<RegistrySkillMeta>,
    exact_slug: Option<RegistrySkillMeta>,
    limit: usize,
) -> Vec<RegistrySkillMeta> {
    let q = normalize_search_key(query);
    let best_score = candidates
        .iter()
        .map(|meta| clawhub_score(meta, query))
        .max()
        .unwrap_or(0);
    if let Some(exact) = exact_slug {
        let exact_id = normalize_search_key(&exact.identifier);
        if !q.is_empty() && (exact_id == q || best_score == 0) {
            if best_score == 0 {
                candidates = vec![exact];
            } else {
                candidates.retain(|meta| meta.identifier != exact.identifier);
                candidates.insert(0, exact);
            }
        }
    }
    candidates.sort_by(|a, b| {
        clawhub_score(b, query)
            .cmp(&clawhub_score(a, query))
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
    candidates.dedup_by(|a, b| a.identifier == b.identifier);
    candidates.truncate(limit);
    candidates
}

pub fn clawhub_latest_version(payload: &Value) -> Option<String> {
    payload
        .get("latestVersion")
        .and_then(|v| value_str(v, &["version"]))
        .or_else(|| value_str(payload, &["version"]))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            payload
                .get("versions")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|v| value_str(v, &["version"]))
                .map(ToString::to_string)
        })
}

pub fn clawhub_file_refs(payload: &Value) -> Vec<ClawHubFileRef> {
    let Some(files) = payload.get("files") else {
        return Vec::new();
    };
    match files {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let path = value_str(item, &["path", "name"])?.trim();
                if path.is_empty() {
                    return None;
                }
                Some(ClawHubFileRef {
                    path: path.to_string(),
                    content: value_str(item, &["content"]).map(ToString::to_string),
                    raw_url: value_str(item, &["rawUrl", "raw_url"]).map(ToString::to_string),
                })
            })
            .collect(),
        Value::Object(map) => map
            .iter()
            .filter_map(|(path, content)| {
                if path.trim().is_empty() {
                    return None;
                }
                Some(ClawHubFileRef {
                    path: path.clone(),
                    content: content.as_str().map(ToString::to_string),
                    raw_url: None,
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// JWT claims for hub authentication
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct HubClaims {
    /// Subject – typically the agent or user ID.
    sub: String,
    /// Issued at (epoch seconds).
    iat: u64,
    /// Expiration (epoch seconds).
    exp: u64,
}

// ---------------------------------------------------------------------------
// SkillsHubClient
// ---------------------------------------------------------------------------

/// HTTP client for the Skills Hub at agentskills.io.
///
/// Uses JWT-based authentication for upload / privileged operations.
/// Download and search are unauthenticated (public).
pub struct SkillsHubClient {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl SkillsHubClient {
    /// Create a new hub client.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_HUB_URL.to_string(),
            api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Create a hub client with a custom base URL (useful for testing).
    pub fn with_base_url(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Generate a JWT token for authenticated requests.
    fn generate_token(&self) -> Result<String, SkillError> {
        use jsonwebtoken::{encode, EncodingKey, Header};
        use std::time::SystemTime;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let claims = HubClaims {
            sub: "hermes-agent".to_string(),
            iat: now,
            exp: now + 3600, // 1 hour
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.api_key.as_bytes()),
        )
        .map_err(|e| SkillError::HubError(format!("JWT encoding failed: {}", e)))
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Search the hub for skills matching `query`, optionally filtered by category.
    #[instrument(skip(self))]
    pub async fn search_skills(
        &self,
        query: &str,
        category: Option<&str>,
    ) -> Result<Vec<SkillMeta>, SkillError> {
        debug!("Searching hub for: {}", query);

        let url = format!("{}/skills/search", self.base_url);
        let body = SearchRequest {
            query: query.to_string(),
            category: category.map(String::from),
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SkillError::HubError(format!("Search request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SkillError::HubError(format!(
                "Search failed ({}): {}",
                status, text
            )));
        }

        let search_resp: SearchResponse = resp
            .json()
            .await
            .map_err(|e| SkillError::HubError(format!("Failed to parse search response: {}", e)))?;

        Ok(search_resp.skills)
    }

    /// Download a skill from the hub by its ID.
    ///
    /// Verifies source integrity if a signature is provided.
    #[instrument(skip(self))]
    pub async fn download_skill(&self, skill_id: &str) -> Result<Skill, SkillError> {
        debug!("Downloading skill from hub: {}", skill_id);

        let url = format!("{}/skills/{}", self.base_url, skill_id);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| SkillError::HubError(format!("Download request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SkillError::HubError(format!(
                "Download failed ({}): {}",
                status, text
            )));
        }

        let dl_resp: DownloadResponse = resp.json().await.map_err(|e| {
            SkillError::HubError(format!("Failed to parse download response: {}", e))
        })?;

        // Verify source integrity if a signature is present.
        if let Some(ref sig) = dl_resp.source_signature {
            verify_skill_signature(&dl_resp.skill, sig)?;
        }

        let guard = crate::guard::SkillGuard::default();
        crate::guard::SkillGuard::validate_structure(&dl_resp.skill)?;
        guard.scan_security_with_policy(&dl_resp.skill, "community")?;

        Ok(dl_resp.skill)
    }

    /// Check if any of the given locally-installed skills have newer
    /// versions available on the hub.
    ///
    /// Sends the local skill names + version hashes to the hub's
    /// `/skills/check-updates` endpoint and returns the list of skills
    /// that have updates available.
    #[instrument(skip(self, installed))]
    pub async fn check_updates(
        &self,
        installed: &[SkillMeta],
    ) -> Result<Vec<SkillUpdate>, SkillError> {
        debug!("Checking updates for {} installed skills", installed.len());

        if installed.is_empty() {
            return Ok(Vec::new());
        }

        let entries: Vec<SkillVersionEntry> = installed
            .iter()
            .map(|m| SkillVersionEntry {
                name: m.name.clone(),
                version: String::new(),
            })
            .collect();

        let url = format!("{}/skills/check-updates", self.base_url);
        let body = CheckUpdatesRequest { skills: entries };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SkillError::HubError(format!("Check-updates request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SkillError::HubError(format!(
                "Check-updates failed ({}): {}",
                status, text
            )));
        }

        let check_resp: CheckUpdatesResponse = resp.json().await.map_err(|e| {
            SkillError::HubError(format!("Failed to parse check-updates response: {}", e))
        })?;

        Ok(check_resp.updates)
    }

    /// Upload a skill to the hub. Returns the hub-assigned ID on success.
    #[instrument(skip(self, skill), fields(name = %skill.name))]
    pub async fn upload_skill(&self, skill: &Skill) -> Result<String, SkillError> {
        debug!("Uploading skill to hub: {}", skill.name);

        let token = self.generate_token()?;
        let url = format!("{}/skills", self.base_url);

        let body = UploadRequest {
            name: skill.name.clone(),
            content: skill.content.clone(),
            category: skill.category.clone(),
            description: skill.description.clone(),
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| SkillError::HubError(format!("Upload request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SkillError::HubError(format!(
                "Upload failed ({}): {}",
                status, text
            )));
        }

        let upload_resp: UploadResponse = resp
            .json()
            .await
            .map_err(|e| SkillError::HubError(format!("Failed to parse upload response: {}", e)))?;

        Ok(upload_resp.id)
    }
}

// ---------------------------------------------------------------------------
// Signature verification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TrustedHubKey {
    key_id: Option<String>,
    key: VerifyingKey,
}

#[derive(Debug, Clone)]
struct ParsedSignature {
    key_id: Option<String>,
    signature: Signature,
}

/// Verify the integrity of a downloaded skill against its source signature.
///
/// Required signature format:
/// - `ed25519:<base64_signature>`
/// - `ed25519:<key_id>:<base64_signature>`
///
/// Trusted verification keys are read from `HERMES_SKILLS_HUB_TRUSTED_KEYS`,
/// a comma-separated list of:
/// - `<base64_or_hex_ed25519_pubkey>`
/// - `<key_id>=<base64_or_hex_ed25519_pubkey>`
fn verify_skill_signature(skill: &Skill, signature: &str) -> Result<(), SkillError> {
    let parsed = parse_ed25519_signature(signature)?;
    let trusted_keys = trusted_hub_public_keys_from_env()?;
    if trusted_keys.is_empty() {
        return Err(SkillError::HubError(format!(
            "Skills hub signature provided but {} is empty; refusing unsigned trust",
            HUB_TRUSTED_KEYS_ENV
        )));
    }
    verify_skill_signature_with_keys(skill, &parsed, &trusted_keys)
}

fn verify_skill_signature_with_keys(
    skill: &Skill,
    parsed: &ParsedSignature,
    trusted_keys: &[TrustedHubKey],
) -> Result<(), SkillError> {
    let payload = signed_skill_payload(skill)?;
    let mut checked = 0usize;
    for trusted in trusted_keys {
        if parsed.key_id.is_some() && trusted.key_id != parsed.key_id {
            continue;
        }
        checked += 1;
        if trusted.key.verify(&payload, &parsed.signature).is_ok() {
            return Ok(());
        }
    }

    if checked == 0 {
        return Err(SkillError::HubError(format!(
            "No trusted hub key matched signature key_id={:?}",
            parsed.key_id
        )));
    }
    Err(SkillError::HubError(
        "Skills hub signature verification failed".to_string(),
    ))
}

fn signed_skill_payload(skill: &Skill) -> Result<Vec<u8>, SkillError> {
    serde_json::to_vec(&serde_json::json!({
        "name": skill.name,
        "content": skill.content,
        "category": skill.category,
        "description": skill.description,
    }))
    .map_err(|e| SkillError::HubError(format!("Failed to build signed payload: {}", e)))
}

fn parse_ed25519_signature(signature: &str) -> Result<ParsedSignature, SkillError> {
    let parts: Vec<&str> = signature.trim().split(':').collect();
    let (key_id, sig_b64) = match parts.as_slice() {
        ["ed25519", sig] => (None, *sig),
        ["ed25519", key_id, sig] if !key_id.is_empty() => (Some((*key_id).to_string()), *sig),
        _ => {
            return Err(SkillError::HubError(
                "Invalid signature format; expected ed25519:<sig> or ed25519:<key_id>:<sig>"
                    .to_string(),
            ))
        }
    };
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.as_bytes())
        .map_err(|e| SkillError::HubError(format!("Invalid base64 signature: {}", e)))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| SkillError::HubError("ed25519 signature must be 64 bytes".to_string()))?;
    Ok(ParsedSignature {
        key_id,
        signature: Signature::from_bytes(&sig_arr),
    })
}

fn trusted_hub_public_keys_from_env() -> Result<Vec<TrustedHubKey>, SkillError> {
    let raw = std::env::var(HUB_TRUSTED_KEYS_ENV).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (key_id, key_raw) = if let Some((id, value)) = entry.split_once('=') {
            (Some(id.trim().to_string()), value.trim())
        } else {
            (None, entry)
        };
        let key_bytes = decode_base64_or_hex_32(key_raw).map_err(|e| {
            SkillError::HubError(format!("Invalid trusted key entry '{}': {}", entry, e))
        })?;
        let key = VerifyingKey::from_bytes(&key_bytes).map_err(|e| {
            SkillError::HubError(format!(
                "Invalid Ed25519 verifying key for entry '{}': {}",
                entry, e
            ))
        })?;
        out.push(TrustedHubKey { key_id, key });
    }
    debug!(
        "Loaded {} trusted skills-hub key(s) from {}",
        out.len(),
        HUB_TRUSTED_KEYS_ENV
    );
    Ok(out)
}

fn decode_base64_or_hex_32(raw: &str) -> Result<[u8; 32], String> {
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(raw.as_bytes()) {
        if bytes.len() == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            return Ok(out);
        }
    }
    if raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        let mut chars = raw.chars();
        for b in &mut out {
            let hi = chars
                .next()
                .ok_or_else(|| "missing high nibble".to_string())?;
            let lo = chars
                .next()
                .ok_or_else(|| "missing low nibble".to_string())?;
            let h = hi
                .to_digit(16)
                .ok_or_else(|| "invalid hex high nibble".to_string())?;
            let l = lo
                .to_digit(16)
                .ok_or_else(|| "invalid hex low nibble".to_string())?;
            *b = ((h << 4) | l) as u8;
        }
        return Ok(out);
    }
    Err("expected base64(32 bytes) or 64-char hex".to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn test_generate_token() {
        let client = SkillsHubClient::new("test-key-12345");
        let token = client.generate_token().unwrap();
        assert!(!token.is_empty());
    }

    #[test]
    fn test_verify_skill_signature_ed25519_ok() {
        let skill = Skill {
            name: "test".to_string(),
            content: "hello".to_string(),
            category: None,
            description: None,
        };
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let payload = signed_skill_payload(&skill).unwrap();
        let signature = signing.sign(&payload);
        let parsed = ParsedSignature {
            key_id: Some("k1".to_string()),
            signature,
        };
        let trusted = vec![TrustedHubKey {
            key_id: Some("k1".to_string()),
            key: signing.verifying_key(),
        }];
        assert!(verify_skill_signature_with_keys(&skill, &parsed, &trusted).is_ok());
    }

    #[test]
    fn test_verify_skill_signature_bad_key_id() {
        let skill = Skill {
            name: "test".to_string(),
            content: "hello".to_string(),
            category: None,
            description: None,
        };
        let signing = SigningKey::from_bytes(&[8u8; 32]);
        let payload = signed_skill_payload(&skill).unwrap();
        let signature = signing.sign(&payload);
        let parsed = ParsedSignature {
            key_id: Some("wrong".to_string()),
            signature,
        };
        let trusted = vec![TrustedHubKey {
            key_id: Some("k1".to_string()),
            key: signing.verifying_key(),
        }];
        assert!(verify_skill_signature_with_keys(&skill, &parsed, &trusted).is_err());
    }

    #[test]
    fn test_verify_skill_signature_tampered_payload() {
        let skill = Skill {
            name: "test".to_string(),
            content: "hello".to_string(),
            category: None,
            description: None,
        };
        let signing = SigningKey::from_bytes(&[9u8; 32]);
        let mut other_skill = skill.clone();
        other_skill.content = "different".to_string();

        let payload = signed_skill_payload(&skill).unwrap();
        let signature = signing.sign(&payload);
        let parsed = ParsedSignature {
            key_id: None,
            signature,
        };
        let trusted = vec![TrustedHubKey {
            key_id: None,
            key: signing.verifying_key(),
        }];
        assert!(verify_skill_signature_with_keys(&other_skill, &parsed, &trusted).is_err());
    }

    #[test]
    fn test_parse_ed25519_signature_formats() {
        let sig = base64::engine::general_purpose::STANDARD.encode([1u8; 64]);
        let with_kid = format!("ed25519:k1:{}", sig);
        let parsed = parse_ed25519_signature(&with_kid).unwrap();
        assert_eq!(parsed.key_id.as_deref(), Some("k1"));

        let without_kid = format!("ed25519:{}", sig);
        let parsed = parse_ed25519_signature(&without_kid).unwrap();
        assert!(parsed.key_id.is_none());
    }

    #[test]
    fn test_parse_ed25519_signature_invalid() {
        assert!(parse_ed25519_signature("deadbeef").is_err());
        assert!(parse_ed25519_signature("ed25519:not-base64").is_err());
    }

    #[test]
    fn test_decode_base64_or_hex_32() {
        let raw = [11u8; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        let got = decode_base64_or_hex_32(&b64).unwrap();
        assert_eq!(got, raw);

        let hex = raw.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let got = decode_base64_or_hex_32(&hex).unwrap();
        assert_eq!(got, raw);
    }

    #[test]
    fn clawhub_listing_maps_display_name_summary_and_tags() {
        let payload = serde_json::json!({
            "items": [
                {
                    "slug": "caldav-calendar",
                    "displayName": "CalDAV Calendar",
                    "summary": "Calendar integration",
                    "tags": ["calendar", "productivity"]
                }
            ]
        });
        let metas = clawhub_metas_from_listing(&payload);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].identifier, "caldav-calendar");
        assert_eq!(metas[0].name, "CalDAV Calendar");
        assert_eq!(metas[0].description, "Calendar integration");
        assert_eq!(metas[0].tags, vec!["calendar", "productivity"]);
    }

    #[test]
    fn clawhub_nested_detail_payload_excludes_latest_tag() {
        let payload = serde_json::json!({
            "skill": {
                "slug": "self-improving-agent",
                "displayName": "self-improving-agent",
                "summary": "Captures learnings and errors for continuous improvement.",
                "tags": {"latest": "3.0.2", "automation": "3.0.2"}
            },
            "latestVersion": {"version": "3.0.2"}
        });
        let meta = clawhub_meta_from_payload(&payload, "self-improving-agent").unwrap();
        assert_eq!(meta.identifier, "self-improving-agent");
        assert_eq!(meta.version.as_deref(), Some("3.0.2"));
        assert_eq!(meta.tags, vec!["automation"]);
        assert!(meta.description.contains("continuous improvement"));
    }

    #[test]
    fn clawhub_finalize_repairs_irrelevant_cached_results_with_exact_slug() {
        let poisoned = RegistrySkillMeta {
            name: "Apple Music DJ".to_string(),
            description: "Unrelated".to_string(),
            source: "clawhub".to_string(),
            identifier: "apple-music-dj".to_string(),
            trust_level: "community".to_string(),
            tags: Vec::new(),
            version: None,
        };
        let exact = RegistrySkillMeta {
            name: "self-improving-agent".to_string(),
            description: "Captures learnings and errors for continuous improvement.".to_string(),
            source: "clawhub".to_string(),
            identifier: "self-improving-agent".to_string(),
            trust_level: "community".to_string(),
            tags: vec!["automation".to_string()],
            version: None,
        };
        let results =
            clawhub_finalize_search_results("self improving", vec![poisoned], Some(exact), 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].identifier, "self-improving-agent");
    }

    #[test]
    fn clawhub_version_and_files_support_inline_and_raw_refs() {
        let detail = serde_json::json!({"latestVersion": {"version": "1.0.1"}});
        assert_eq!(clawhub_latest_version(&detail).as_deref(), Some("1.0.1"));
        let version_payload = serde_json::json!({
            "files": [
                {"path": "SKILL.md", "rawUrl": "https://files.example/skill-md"},
                {"path": "README.md", "content": "hello"}
            ]
        });
        let refs = clawhub_file_refs(&version_payload);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].path, "SKILL.md");
        assert_eq!(
            refs[0].raw_url.as_deref(),
            Some("https://files.example/skill-md")
        );
        assert_eq!(refs[1].content.as_deref(), Some("hello"));

        let object_payload = serde_json::json!({"files": {"SKILL.md": "# Skill"}});
        let refs = clawhub_file_refs(&object_payload);
        assert_eq!(refs[0].path, "SKILL.md");
        assert_eq!(refs[0].content.as_deref(), Some("# Skill"));
    }

    #[test]
    fn test_default_hub_url() {
        assert_eq!(DEFAULT_HUB_URL, "https://agentskills.io/api/v1");
    }
}
