fn normalize_tap_path_for_storage(path: &str) -> String {
    let normalized = path.trim_matches('/');
    if normalized.is_empty() {
        String::new()
    } else {
        format!("{}/", normalized)
    }
}

fn tap_object_to_string(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
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

fn tap_string_to_object(tap: &str) -> serde_json::Value {
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

fn read_skill_taps(path: &std::path::Path) -> Vec<String> {
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

fn subscription_entry_to_source(entry: &serde_json::Value) -> Option<String> {
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

fn read_skill_subscriptions(path: &std::path::Path) -> Vec<String> {
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

fn write_skill_taps(path: &std::path::Path, taps: &[String]) -> Result<(), AgentError> {
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

fn merged_skill_taps(custom_taps: &[String]) -> Vec<String> {
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

fn subscription_source_to_tap(source: &str) -> Option<String> {
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

fn effective_skill_taps(
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

