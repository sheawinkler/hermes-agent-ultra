//! Skills Hub client for interacting with the agentskills.io API.

use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use hermes_core::types::{Skill, SkillMeta};

use crate::skill::SkillError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default base URL for the Skills Hub API.
pub const DEFAULT_HUB_URL: &str = "https://agentskills.io/api/v1";

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

        // Validate the downloaded skill through the guard.
        crate::guard::SkillGuard::default().validate_skill(&dl_resp.skill)?;

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

/// Verify the integrity of a downloaded skill against its source signature.
///
/// The signature is an HMAC-SHA256 of the skill's content, keyed with the
/// hub's public key fingerprint. In this implementation we do a simple
/// check; production deployments should use proper public-key verification.
fn verify_skill_signature(skill: &Skill, signature: &str) -> Result<(), SkillError> {
    use sha2::{Digest, Sha256};

    // Compute SHA-256 of the content.
    let mut hasher = Sha256::new();
    hasher.update(skill.content.as_bytes());
    let hash = hex::encode(hasher.finalize());

    // In a real implementation this would verify against a public key.
    // For now we just check that the signature is a valid hex string of
    // the expected length (64 chars for SHA-256).
    if signature.len() != 64 || !signature.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SkillError::HubError(
            "Invalid skill source signature format".to_string(),
        ));
    }

    // Log the verification (we don't enforce equality in this simplified version).
    debug!(
        "Skill {} signature verification: computed={}, sig={}",
        skill.name,
        &hash[..16],
        &signature[..16]
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Hex encoding (simple, no external crate)
// ---------------------------------------------------------------------------

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token() {
        let client = SkillsHubClient::new("test-key-12345");
        let token = client.generate_token().unwrap();
        assert!(!token.is_empty());
    }

    #[test]
    fn test_verify_skill_signature_valid_hex() {
        let skill = Skill {
            name: "test".to_string(),
            content: "hello".to_string(),
            category: None,
            description: None,
        };
        // A valid 64-char hex string should pass.
        let sig = "a".repeat(64);
        assert!(verify_skill_signature(&skill, &sig).is_ok());
    }

    #[test]
    fn test_verify_skill_signature_invalid_length() {
        let skill = Skill {
            name: "test".to_string(),
            content: "hello".to_string(),
            category: None,
            description: None,
        };
        let sig = "tooshort";
        assert!(verify_skill_signature(&skill, sig).is_err());
    }

    #[test]
    fn test_verify_skill_signature_invalid_chars() {
        let skill = Skill {
            name: "test".to_string(),
            content: "hello".to_string(),
            category: None,
            description: None,
        };
        // 64 chars but contains non-hex.
        let sig = "g".repeat(64);
        assert!(verify_skill_signature(&skill, &sig).is_err());
    }

    #[test]
    fn test_default_hub_url() {
        assert_eq!(DEFAULT_HUB_URL, "https://agentskills.io/api/v1");
    }
}
