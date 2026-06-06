//! Shared OAuth runtime gate helpers.
//!
//! CLI slash commands and agent-callable tools use these primitives so provider
//! capability checks and minimum-runtime decisions cannot drift.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::providers::{canonical_provider_id, provider_capability_for};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthRuntimeGateManifest {
    #[serde(default = "oauth_runtime_gate_default_min_version")]
    pub default_min_version: String,
    #[serde(default)]
    pub required_oauth_provider_ids: Vec<String>,
    #[serde(default)]
    pub provider_min_versions: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OAuthRuntimeGate {
    pub ok: bool,
    pub provider: String,
    pub runtime_version: String,
    pub required_min_version: String,
    pub manifest_source: String,
    pub detail: String,
}

pub fn oauth_runtime_gate_default_min_version() -> String {
    "0.1.0".to_string()
}

pub fn oauth_runtime_gate_manifest_default() -> OAuthRuntimeGateManifest {
    OAuthRuntimeGateManifest {
        default_min_version: oauth_runtime_gate_default_min_version(),
        required_oauth_provider_ids: vec![
            "anthropic".to_string(),
            "nous".to_string(),
            "openai-codex".to_string(),
            "qwen-oauth".to_string(),
            "google-gemini-cli".to_string(),
        ],
        provider_min_versions: HashMap::new(),
    }
}

pub fn normalize_oauth_runtime_gate_manifest(
    manifest: OAuthRuntimeGateManifest,
) -> OAuthRuntimeGateManifest {
    let mut out = manifest;
    if out.default_min_version.trim().is_empty() {
        out.default_min_version = oauth_runtime_gate_default_min_version();
    }
    out.required_oauth_provider_ids = out
        .required_oauth_provider_ids
        .into_iter()
        .map(|v| canonical_provider_id(v.trim()))
        .filter(|v| !v.trim().is_empty())
        .collect();
    let mut mins = HashMap::new();
    for (provider, version) in out.provider_min_versions {
        let key = canonical_provider_id(provider.trim());
        if key.is_empty() || version.trim().is_empty() {
            continue;
        }
        mins.insert(key, version.trim().to_string());
    }
    out.provider_min_versions = mins;
    out
}

pub fn load_oauth_runtime_gate_manifest_from_path(path: &Path) -> Option<OAuthRuntimeGateManifest> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<OAuthRuntimeGateManifest>(&raw).ok()?;
    Some(normalize_oauth_runtime_gate_manifest(parsed))
}

pub fn parse_version_triplet(raw: &str) -> Option<(u64, u64, u64)> {
    let mut parts = raw.trim().split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch_raw = parts.next().unwrap_or("0");
    let patch = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<u64>()
        .ok()?;
    Some((major, minor, patch))
}

pub fn version_at_least(current: &str, minimum: &str) -> bool {
    let Some(cur) = parse_version_triplet(current) else {
        return false;
    };
    let Some(min) = parse_version_triplet(minimum) else {
        return false;
    };
    cur >= min
}

pub fn oauth_min_version_for_provider(
    provider: &str,
    manifest: &OAuthRuntimeGateManifest,
) -> Option<String> {
    let normalized = canonical_provider_id(provider);
    if !provider_capability_for(&normalized)?.oauth_supported {
        return None;
    }
    Some(
        manifest
            .provider_min_versions
            .get(&normalized)
            .cloned()
            .unwrap_or_else(|| manifest.default_min_version.clone()),
    )
}

pub fn oauth_runtime_gate_for_provider(
    provider: &str,
    runtime_version: &str,
    manifest: &OAuthRuntimeGateManifest,
    manifest_source: impl Into<String>,
) -> Option<OAuthRuntimeGate> {
    let normalized = canonical_provider_id(provider);
    let required_min_version = oauth_min_version_for_provider(&normalized, manifest)?;
    let manifest_source = manifest_source.into();
    let ok = version_at_least(runtime_version, &required_min_version);
    Some(OAuthRuntimeGate {
        ok,
        provider: normalized,
        runtime_version: runtime_version.to_string(),
        required_min_version: required_min_version.clone(),
        manifest_source: manifest_source.clone(),
        detail: format!(
            "runtime={} required>={} manifest={}",
            runtime_version, required_min_version, manifest_source
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_triplet_handles_partial_and_suffix_versions() {
        assert_eq!(parse_version_triplet("1"), Some((1, 0, 0)));
        assert_eq!(parse_version_triplet("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version_triplet("1.2.3-beta"), Some((1, 2, 3)));
        assert!(version_at_least("1.2.3-beta", "1.2.0"));
        assert!(!version_at_least("1.2.3", "1.3.0"));
    }

    #[test]
    fn manifest_normalization_canonicalizes_provider_aliases() {
        let manifest = normalize_oauth_runtime_gate_manifest(OAuthRuntimeGateManifest {
            default_min_version: "".to_string(),
            required_oauth_provider_ids: vec!["codex".to_string(), "gemini-cli".to_string()],
            provider_min_versions: HashMap::from([("claude".to_string(), "2.0.0".to_string())]),
        });

        assert_eq!(manifest.default_min_version, "0.1.0");
        assert_eq!(
            manifest.required_oauth_provider_ids,
            vec!["openai-codex", "google-gemini-cli"]
        );
        assert_eq!(
            manifest.provider_min_versions.get("anthropic"),
            Some(&"2.0.0".to_string())
        );
    }

    #[test]
    fn oauth_gate_skips_non_oauth_providers_and_blocks_old_runtime() {
        let manifest = normalize_oauth_runtime_gate_manifest(OAuthRuntimeGateManifest {
            default_min_version: "99.0.0".to_string(),
            required_oauth_provider_ids: vec!["nous".to_string()],
            provider_min_versions: HashMap::new(),
        });

        assert!(
            oauth_runtime_gate_for_provider("openrouter", "0.15.0", &manifest, "test").is_none()
        );
        let gate = oauth_runtime_gate_for_provider("nous", "0.15.0", &manifest, "test")
            .expect("oauth gate");
        assert!(!gate.ok);
        assert_eq!(gate.required_min_version, "99.0.0");
        assert!(gate.detail.contains("manifest=test"));
    }
}
