//! Skill versioning: hash-based version computation and comparison.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// SkillVersion
// ---------------------------------------------------------------------------

/// A version record for a skill, tracking when it was created and modified.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillVersion {
    /// Semantic or hash-based version string (e.g. "0.1.0" or "sha256:a1b2c3...").
    pub version: String,
    /// When this version was first created.
    pub created_at: DateTime<Utc>,
    /// When this version was last updated.
    pub updated_at: DateTime<Utc>,
    /// Optional changelog entry describing the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

// ---------------------------------------------------------------------------
// Version computation
// ---------------------------------------------------------------------------

/// Compute a deterministic version string from skill content using SHA-256.
///
/// The result is prefixed with `sha256:` and truncated to 16 hex chars for
/// readability while still providing strong collision resistance for
/// content changes.
pub fn compute_version(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hash = hasher.finalize();
    let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
    format!("sha256:{}", &hex[..16])
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

/// Compare two version strings.
///
/// Supports two formats:
/// - **Semantic versions** (e.g. "1.2.3"): compared component-by-component.
/// - **Hash-based versions** (e.g. "sha256:abcdef"): compared
///   lexicographically (not meaningful for ordering, but consistent).
/// - Mixed formats: semantic versions sort before hash-based versions.
pub fn compare_versions(v1: &str, v2: &str) -> Ordering {
    let v1_is_semver = is_semver(v1);
    let v2_is_semver = is_semver(v2);

    // If one is semver and the other is hash-based, semver sorts first.
    match (v1_is_semver, v2_is_semver) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }

    if v1_is_semver && v2_is_semver {
        compare_semver(v1, v2)
    } else {
        // Both are hash-based or unknown – fall back to lexicographic.
        v1.cmp(v2)
    }
}

/// Check whether a version string looks like a semantic version (x.y.z).
fn is_semver(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() >= 2 && parts.iter().all(|p| p.parse::<u32>().is_ok())
}

/// Compare two semantic version strings component-by-component.
fn compare_semver(v1: &str, v2: &str) -> Ordering {
    let p1: Vec<u32> = v1.split('.').filter_map(|s| s.parse().ok()).collect();
    let p2: Vec<u32> = v2.split('.').filter_map(|s| s.parse().ok()).collect();

    for (a, b) in p1.iter().zip(p2.iter()) {
        match a.cmp(b) {
            Ordering::Equal => continue,
            other => return other,
        }
    }

    // If all compared components are equal, the longer version is greater.
    p1.len().cmp(&p2.len())
}

// ---------------------------------------------------------------------------
// Change tracking (self-improvement)
// ---------------------------------------------------------------------------

/// A record of a skill change, used for self-improvement tracking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillChange {
    /// Name of the skill that changed.
    pub skill_name: String,
    /// Version before the change.
    pub from_version: String,
    /// Version after the change.
    pub to_version: String,
    /// When the change happened.
    pub timestamp: DateTime<Utc>,
    /// Optional description of what changed.
    pub description: Option<String>,
}

/// Compute a `SkillChange` record from before/after content.
pub fn track_change(
    skill_name: &str,
    old_content: &str,
    new_content: &str,
    description: Option<&str>,
) -> SkillChange {
    SkillChange {
        skill_name: skill_name.to_string(),
        from_version: compute_version(old_content),
        to_version: compute_version(new_content),
        timestamp: Utc::now(),
        description: description.map(String::from),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_version_deterministic() {
        let v1 = compute_version("hello world");
        let v2 = compute_version("hello world");
        assert_eq!(v1, v2);
    }

    #[test]
    fn test_compute_version_different_content() {
        let v1 = compute_version("hello");
        let v2 = compute_version("world");
        assert_ne!(v1, v2);
    }

    #[test]
    fn test_compute_version_format() {
        let v = compute_version("test");
        assert!(v.starts_with("sha256:"));
        assert_eq!(v.len(), 16 + 7); // "sha256:" + 16 hex chars
    }

    #[test]
    fn test_compare_semver_equal() {
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn test_compare_semver_major() {
        assert_eq!(compare_versions("2.0.0", "1.0.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "2.0.0"), Ordering::Less);
    }

    #[test]
    fn test_compare_semver_minor() {
        assert_eq!(compare_versions("1.2.0", "1.1.0"), Ordering::Greater);
    }

    #[test]
    fn test_compare_semver_patch() {
        assert_eq!(compare_versions("1.0.3", "1.0.2"), Ordering::Greater);
    }

    #[test]
    fn test_compare_semver_different_lengths() {
        assert_eq!(compare_versions("1.2", "1.1.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.0", "1.2"), Ordering::Greater);
    }

    #[test]
    fn test_compare_hash_versions() {
        let v1 = "sha256:aaaa";
        let v2 = "sha256:bbbb";
        assert_eq!(compare_versions(v1, v2), Ordering::Less);
    }

    #[test]
    fn test_compare_mixed() {
        // Semantic versions sort before hash-based ones.
        assert_eq!(compare_versions("1.0.0", "sha256:aaaa"), Ordering::Less);
        assert_eq!(compare_versions("sha256:aaaa", "1.0.0"), Ordering::Greater);
    }

    #[test]
    fn test_track_change() {
        let change = track_change("my-skill", "old content", "new content", Some("updated steps"));
        assert_eq!(change.skill_name, "my-skill");
        assert_eq!(change.from_version, compute_version("old content"));
        assert_eq!(change.to_version, compute_version("new content"));
        assert_eq!(change.description, Some("updated steps".to_string()));
    }

    #[test]
    fn test_is_semver() {
        assert!(is_semver("1.0.0"));
        assert!(is_semver("0.1"));
        assert!(!is_semver("sha256:abc"));
        assert!(!is_semver("latest"));
    }
}