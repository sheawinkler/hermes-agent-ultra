//! Skill tap resolution and tap file management.

use hermes_core::AgentError;

use super::constants::DEFAULT_SKILL_TAPS;
use super::github::{github_default_branch, github_repo_tree};
use super::parse::parse_skill_tap_spec;
use super::types::ResolvedSkillSource;

pub(crate) async fn resolve_skill_via_taps(
    client: &reqwest::Client,
    taps: &[String],
    requested_skill: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let mut suggestions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for tap in taps {
        let Some(spec) = parse_skill_tap_spec(tap) else {
            continue;
        };
        let branch = match github_default_branch(client, &spec.repo).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tree = match github_repo_tree(client, &spec.repo, &branch).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path_prefix = if spec.path.is_empty() {
            String::new()
        } else {
            format!("{}/", spec.path.trim_matches('/'))
        };
        for entry in tree {
            if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
                continue;
            }
            if !path_prefix.is_empty() && !entry.path.starts_with(&path_prefix) {
                continue;
            }
            let skill_dir = entry.path.trim_end_matches("/SKILL.md");
            let skill_name = skill_dir
                .split('/')
                .next_back()
                .unwrap_or(skill_dir)
                .to_string();
            if skill_name.eq_ignore_ascii_case(requested_skill) {
                return Ok(ResolvedSkillSource {
                    repo: spec.repo.clone(),
                    branch,
                    skill_dir: skill_dir.to_string(),
                });
            }
            if skill_name
                .to_ascii_lowercase()
                .contains(&requested_skill.to_ascii_lowercase())
            {
                suggestions.insert(skill_name);
            }
        }
    }

    let suggestion_text = if suggestions.is_empty() {
        "none".to_string()
    } else {
        suggestions
            .into_iter()
            .take(8)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(AgentError::Config(format!(
        "Skill '{}' not found in configured taps. Suggestions: {}",
        requested_skill, suggestion_text
    )))
}

pub(crate) async fn resolve_skill_in_repo(
    client: &reqwest::Client,
    repo: &str,
    requested_skill: &str,
    preferred_prefix: Option<&str>,
) -> Result<ResolvedSkillSource, AgentError> {
    let branch = github_default_branch(client, repo).await?;
    let tree = github_repo_tree(client, repo, &branch).await?;

    let preferred_prefix = preferred_prefix
        .map(|v| v.trim_matches('/').to_string())
        .unwrap_or_default();
    let mut exact_candidates: Vec<String> = Vec::new();
    let mut fuzzy_candidates: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    for entry in tree {
        if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
            continue;
        }
        let skill_dir = entry.path.trim_end_matches("/SKILL.md").to_string();
        let skill_name = skill_dir
            .split('/')
            .next_back()
            .unwrap_or(skill_dir.as_str())
            .to_string();
        if skill_name.eq_ignore_ascii_case(requested_skill) {
            exact_candidates.push(skill_dir.clone());
        } else if skill_name
            .to_ascii_lowercase()
            .contains(&requested_skill.to_ascii_lowercase())
        {
            fuzzy_candidates.insert(skill_name);
        }
    }

    if exact_candidates.is_empty() {
        let suggestion_text = if fuzzy_candidates.is_empty() {
            "none".to_string()
        } else {
            fuzzy_candidates
                .into_iter()
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(AgentError::Config(format!(
            "Skill '{}' not found in repo {}. Suggestions: {}",
            requested_skill, repo, suggestion_text
        )));
    }

    exact_candidates.sort_by_key(|candidate| {
        let preferred = if preferred_prefix.is_empty() {
            1usize
        } else if candidate.starts_with(&format!("{}/", preferred_prefix)) {
            0usize
        } else {
            1usize
        };
        (preferred, candidate.len(), candidate.clone())
    });
    let skill_dir = exact_candidates
        .into_iter()
        .next()
        .ok_or_else(|| AgentError::Config("No matching skill path found.".into()))?;

    Ok(ResolvedSkillSource {
        repo: repo.to_string(),
        branch,
        skill_dir,
    })
}

pub(crate) async fn search_skills_via_taps(
    client: &reqwest::Client,
    taps: &[String],
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, AgentError> {
    let query_l = query.to_ascii_lowercase();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<(String, String)> = Vec::new();

    for tap in taps {
        let Some(spec) = parse_skill_tap_spec(tap) else {
            continue;
        };
        let branch = match github_default_branch(client, &spec.repo).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tree = match github_repo_tree(client, &spec.repo, &branch).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path_prefix = if spec.path.is_empty() {
            String::new()
        } else {
            format!("{}/", spec.path.trim_matches('/'))
        };
        for entry in tree {
            if entry.kind != "blob" || !entry.path.ends_with("/SKILL.md") {
                continue;
            }
            if !path_prefix.is_empty() && !entry.path.starts_with(&path_prefix) {
                continue;
            }
            let skill_dir = entry.path.trim_end_matches("/SKILL.md");
            let skill_name = skill_dir.split('/').next_back().unwrap_or(skill_dir);
            if !skill_name.to_ascii_lowercase().contains(&query_l) {
                continue;
            }
            let key = format!("{}/{}", spec.repo, skill_dir);
            if seen.insert(key.clone()) {
                out.push((skill_name.to_string(), key));
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Tap management
// ---------------------------------------------------------------------------

pub(crate) fn normalize_tap_path_for_storage(path: &str) -> String {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        String::new()
    } else {
        format!("{}/", normalized)
    }
}

pub(crate) fn tap_object_to_string(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    if let Some(url) = obj
        .get("url")
        .and_then(|u| u.as_str())
        .or_else(|| obj.get("tap").and_then(|u| u.as_str()))
    {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let repo = obj.get("repo").and_then(|v| v.as_str())?;
    let repo = repo.trim().trim_matches('/');
    if repo.is_empty() {
        return None;
    }
    let path = obj
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("skills/")
        .trim()
        .trim_matches('/');
    if path.is_empty() {
        Some(format!("https://github.com/{}::", repo))
    } else {
        Some(format!("https://github.com/{}::{}", repo, path))
    }
}

pub(crate) fn tap_string_to_object(tap: &str) -> serde_json::Value {
    if let Some(spec) = parse_skill_tap_spec(tap) {
        let mut obj = serde_json::Map::new();
        obj.insert("repo".to_string(), serde_json::Value::String(spec.repo));
        obj.insert(
            "path".to_string(),
            serde_json::Value::String(normalize_tap_path_for_storage(&spec.path)),
        );
        obj.insert(
            "url".to_string(),
            serde_json::Value::String(tap.to_string()),
        );
        serde_json::Value::Object(obj)
    } else {
        serde_json::json!({ "url": tap })
    }
}

pub(crate) fn read_skill_taps(path: &std::path::Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
    let Ok(value) = parsed else {
        return Vec::new();
    };
    match value {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        serde_json::Value::Object(map) => {
            let taps = map.get("taps").cloned().unwrap_or(serde_json::Value::Null);
            match taps {
                serde_json::Value::Array(arr) => arr
                    .into_iter()
                    .filter_map(|item| match item {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Object(obj) => tap_object_to_string(&obj),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

pub(crate) fn subscription_entry_to_source(entry: &serde_json::Value) -> Option<String> {
    match entry {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Object(obj) => {
            let source = obj
                .get("source")
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("tap").and_then(|v| v.as_str()))
                .or_else(|| obj.get("url").and_then(|v| v.as_str()))?;
            let trimmed = source.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        _ => None,
    }
}

pub(crate) fn read_skill_subscriptions(path: &std::path::Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
    let Ok(value) = parsed else {
        return Vec::new();
    };
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(subscription_entry_to_source)
            .collect(),
        serde_json::Value::Object(map) => {
            let subscriptions = map
                .get("subscriptions")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            match subscriptions {
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(subscription_entry_to_source)
                    .collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

pub(crate) fn write_skill_taps(path: &std::path::Path, taps: &[String]) -> Result<(), AgentError> {
    let serialized_taps: Vec<serde_json::Value> =
        taps.iter().map(|tap| tap_string_to_object(tap)).collect();
    let payload = serde_json::json!({
        "taps": serialized_taps
    });
    let json =
        serde_json::to_string_pretty(&payload).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, format!("{}\n", json)).map_err(|e| AgentError::Io(e.to_string()))?;
    Ok(())
}

pub(crate) fn merged_skill_taps(custom_taps: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for tap in DEFAULT_SKILL_TAPS {
        merged.push((*tap).to_string());
    }
    for tap in custom_taps {
        if !merged.iter().any(|existing| existing == tap) {
            merged.push(tap.clone());
        }
    }
    merged
}

pub(crate) fn subscription_source_to_tap(source: &str) -> Option<String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://github.com/") || lower.starts_with("http://github.com/") {
        return parse_skill_tap_spec(trimmed).map(|_| trimmed.to_string());
    }
    if lower.contains("://") {
        return None;
    }
    if let Some((prefix, _)) = trimmed.split_once('/') {
        let p = prefix.trim().to_ascii_lowercase();
        if matches!(
            p.as_str(),
            "official" | "skills.sh" | "lobehub" | "clawhub" | "claude-marketplace" | "github"
        ) {
            return None;
        }
    }
    parse_skill_tap_spec(trimmed).map(|_| trimmed.to_string())
}

pub(crate) fn effective_skill_taps(
    taps_file: &std::path::Path,
    subscriptions_file: &std::path::Path,
) -> Vec<String> {
    let custom_taps = read_skill_taps(taps_file);
    let mut merged = merged_skill_taps(&custom_taps);
    for sub in read_skill_subscriptions(subscriptions_file) {
        // Subscriptions may include non-tap registries; only include values that
        // can be interpreted as GitHub tap specs.
        let Some(tap) = subscription_source_to_tap(&sub) else {
            continue;
        };
        if !merged.iter().any(|existing| existing == &tap) {
            merged.push(tap);
        }
    }
    merged
}
