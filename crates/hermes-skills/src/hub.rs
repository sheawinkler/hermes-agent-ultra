//! Skills Hub client for interacting with the agentskills.io API.

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

    /// Downloading a legacy single-file hub response is intentionally not an
    /// admission path. Remote effects must flow through the package installer,
    /// which binds a v2 manifest, canonical bundle digest, trusted keyring,
    /// scan evidence, compatibility, and revocations.
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

        let _: Value = resp.json().await.map_err(|e| {
            SkillError::HubError(format!("Failed to parse download response: {}", e))
        })?;
        Err(SkillError::HubError(
            "unmanifested hub download is not admissible; install a signed v2 package through the trust verifier"
                .to_string(),
        ))
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
