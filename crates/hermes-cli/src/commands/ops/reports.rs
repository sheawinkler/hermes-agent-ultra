use std::path::{Path, PathBuf};

use super::super::read_json_file;

pub(crate) fn latest_json_report(report_dir: &Path, prefix: &str) -> Option<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(report_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_string_lossy();
            if name.starts_with(prefix) && name.ends_with(".json") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    reports.sort();
    reports.into_iter().last()
}

pub(crate) fn summarize_gate_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(|v| v.as_bool())
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    Some(format!(
        "{}={} @ {} ({})",
        key,
        ok,
        generated,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    ))
}

pub(crate) fn summarize_self_evolution_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(|v| v.as_bool())
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let idx = report
        .get("summary")
        .and_then(|s| s.get("intelligence_index"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let recs = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    Some(format!(
        "{}={} idx={:.2} recs={} @ {} ({})",
        key,
        ok,
        idx,
        recs,
        generated,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    ))
}

pub(crate) fn self_evolution_recommendations(path: &Path) -> Vec<String> {
    let report = match read_json_file(path) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let Some(items) = report.get("recommendations").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
            let sev = obj.get("severity").and_then(|v| v.as_str()).unwrap_or("PX");
            let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let cmd = obj.get("command").and_then(|v| v.as_str()).unwrap_or("");
            Some(format!("[{sev}] {id}: {title}\n  cmd: {cmd}"))
        })
        .collect()
}

pub(crate) fn summarize_performance_autopilot_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(|v| v.as_bool())
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let recommendations = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let severe = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|item| {
                    item.get("severity")
                        .and_then(|v| v.as_str())
                        .is_some_and(|sev| {
                            sev.eq_ignore_ascii_case("P0") || sev.eq_ignore_ascii_case("P1")
                        })
                })
                .count()
        })
        .unwrap_or(0);
    let adaptive_idx = report
        .get("adaptive_index")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let profile = report
        .get("profile_recommendation")
        .and_then(|v| v.as_str())
        .unwrap_or("balanced");
    Some(format!(
        "{}={} idx={:.2} profile={} recs={} severe={} @ {} ({})",
        key,
        ok,
        adaptive_idx,
        profile,
        recommendations,
        severe,
        generated,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    ))
}

pub(crate) fn performance_autopilot_recommendations(path: &Path) -> Vec<String> {
    let report = match read_json_file(path) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let Some(items) = report.get("recommendations").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
            let sev = obj.get("severity").and_then(|v| v.as_str()).unwrap_or("PX");
            let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let rec = obj
                .get("recommendation")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(format!("[{sev}] {id}: {title}\n  recommendation: {rec}"))
        })
        .collect()
}
