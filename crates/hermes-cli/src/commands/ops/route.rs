use std::path::{Path, PathBuf};

use super::super::read_json_file;

pub(crate) fn route_learning_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-learning.json")
}

pub(crate) fn route_health_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-health.json")
}

pub(crate) fn route_autotune_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-autotune.json")
}

pub(crate) fn route_autotune_env_path() -> PathBuf {
    hermes_config::hermes_home().join("route-autotune.env")
}

pub(crate) fn summarize_route_health_state(path: &Path) -> String {
    let Some(report) = read_json_file(path) else {
        return "route_health=unknown".to_string();
    };
    let overall = report
        .get("overall")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let score = report
        .get("summary")
        .and_then(|v| v.get("health_score"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    format!(
        "route_health={} score={:.2} @ {}",
        overall, score, generated
    )
}

pub(crate) fn summarize_route_health_details(path: &Path) -> Option<String> {
    let report = read_json_file(path)?;
    let entries = report.get("entries")?.as_array()?;
    if entries.is_empty() {
        return Some("route_health_trace=no_entries".to_string());
    }
    let mut ranked = entries
        .iter()
        .filter_map(|entry| {
            let key = entry.get("key").and_then(|v| v.as_str())?;
            let tier = entry
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let health = entry
                .get("health_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reasons = entry
                .get("reasons")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some((key.to_string(), tier.to_string(), health, reasons))
        })
        .collect::<Vec<_>>();
    if ranked.is_empty() {
        return Some("route_health_trace=no_parseable_entries".to_string());
    }
    ranked.sort_by(|a, b| a.2.total_cmp(&b.2));
    let hottest = ranked
        .iter()
        .take(3)
        .map(|(key, tier, health, reasons)| {
            let reason_text = if reasons.is_empty() {
                "no_reasons".to_string()
            } else {
                reasons.join("|")
            };
            format!("{key} tier={tier} score={health:.2} reasons={reason_text}")
        })
        .collect::<Vec<_>>()
        .join(" ; ");
    Some(format!("route_health_trace={}", hottest))
}
