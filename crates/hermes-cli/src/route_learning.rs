//! Route learning, health, and autotune logic.
//!
//! Extracted from `main.rs` to keep the entry point focused on CLI dispatch
//! and gateway lifecycle. All items here are `pub(crate)` so they remain
//! visible to the rest of the binary crate.

use crate::doctor::build_doctor_support_bundle_with_options;
use crate::doctor::build_elite_doctor_diagnostics;
use crate::doctor::write_doctor_snapshot;
use crate::hermes_state_root;
use crate::provenance::sign_artifact_bytes;
use crate::provenance::write_provenance_sidecar;
use hermes_cli::paths::CliStateRoot;
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct RouteLearningStatsRecord {
    pub(crate) samples: u32,
    pub(crate) success_rate: f64,
    pub(crate) avg_latency_ms: f64,
    pub(crate) consecutive_failures: u32,
    pub(crate) updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct RouteLearningStateRecord {
    pub(crate) schema_version: u32,
    pub(crate) saved_at_unix_ms: i64,
    pub(crate) entries: HashMap<String, RouteLearningStatsRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RouteHealthEntry {
    pub(crate) key: String,
    pub(crate) health_score: f64,
    pub(crate) tier: String,
    pub(crate) reasons: Vec<String>,
    pub(crate) stats: RouteLearningStatsRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RouteAutotunePlan {
    pub(crate) generated_at: String,
    pub(crate) learning_path: String,
    pub(crate) health_report_path: String,
    pub(crate) env_path: String,
    pub(crate) summary: serde_json::Value,
    pub(crate) confidence: String,
    pub(crate) reasons: Vec<String>,
    pub(crate) overrides: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const ROUTE_AUTOTUNE_ENV_KEYS: &[&str] = &[
    "HERMES_SMART_ROUTING_LEARNING_ALPHA",
    "HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS",
    "HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN",
    "HERMES_SMART_ROUTING_LEARNING_TTL_SECS",
    "HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS",
];

// ---------------------------------------------------------------------------
// Route-learning helpers
// ---------------------------------------------------------------------------

pub(crate) fn route_learning_ttl_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(7 * 24 * 60 * 60)
}

pub(crate) fn route_learning_half_life_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(24 * 60 * 60)
}

pub(crate) fn route_learning_effective_stats(
    stats: &RouteLearningStatsRecord,
    now_ms: i64,
) -> Option<RouteLearningStatsRecord> {
    if stats.samples == 0 {
        return None;
    }
    let mut out = stats.clone();
    if out.updated_at_unix_ms <= 0 {
        return Some(out);
    }
    let age_ms = now_ms.saturating_sub(out.updated_at_unix_ms).max(0);
    let ttl_secs = route_learning_ttl_secs();
    if ttl_secs > 0 && age_ms >= ttl_secs.saturating_mul(1000) {
        return None;
    }
    let half_life_secs = route_learning_half_life_secs();
    if half_life_secs <= 0 || age_ms <= 0 {
        return Some(out);
    }
    let half_life_ms = (half_life_secs.saturating_mul(1000)) as f64;
    let decay = (0.5_f64)
        .powf((age_ms as f64) / half_life_ms)
        .clamp(0.0, 1.0);
    let baseline_success = 0.90;
    let baseline_latency = 1800.0;
    out.success_rate = baseline_success + (out.success_rate - baseline_success) * decay;
    out.avg_latency_ms = baseline_latency + (out.avg_latency_ms - baseline_latency) * decay;
    out.consecutive_failures = ((out.consecutive_failures as f64) * decay).round() as u32;
    out.samples = ((out.samples as f64) * decay).round().max(1.0) as u32;
    Some(out)
}

pub(crate) fn route_learning_score(stats: &RouteLearningStatsRecord) -> f64 {
    let success_rate = stats.success_rate;
    let latency_score = (1.0 / (1.0 + (stats.avg_latency_ms / 2500.0))).clamp(0.05, 1.0);
    let failure_penalty = (stats.consecutive_failures as f64 * 0.08).min(0.35);
    let exploration_bonus = {
        let coverage = (stats.samples.min(20) as f64) / 20.0;
        (1.0 - coverage) * 0.03
    };
    (success_rate * 0.60) + (latency_score * 0.30) + exploration_bonus - failure_penalty
}

pub(crate) fn load_route_learning_state_for_cli(
    path: &Path,
) -> Result<RouteLearningStateRecord, AgentError> {
    if !path.exists() {
        return Ok(RouteLearningStateRecord {
            schema_version: 1,
            saved_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            entries: HashMap::new(),
        });
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

// ---------------------------------------------------------------------------
// Clamp helpers
// ---------------------------------------------------------------------------

pub(crate) fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

pub(crate) fn clamp_i64(value: i64, min: i64, max: i64) -> i64 {
    value.max(min).min(max)
}

// ---------------------------------------------------------------------------
// Route health tier
// ---------------------------------------------------------------------------

pub(crate) fn route_health_tier(
    stats: &RouteLearningStatsRecord,
    score: f64,
) -> (String, Vec<String>, f64) {
    let mut reasons = Vec::new();
    if stats.success_rate < 0.55 {
        reasons.push("low_success_rate".to_string());
    } else if stats.success_rate < 0.72 {
        reasons.push("recovering_success_rate".to_string());
    }
    if stats.consecutive_failures >= 5 {
        reasons.push("failure_streak_critical".to_string());
    } else if stats.consecutive_failures >= 3 {
        reasons.push("failure_streak_watch".to_string());
    }
    if stats.avg_latency_ms > 5000.0 {
        reasons.push("high_latency".to_string());
    } else if stats.avg_latency_ms > 3000.0 {
        reasons.push("latency_watch".to_string());
    }

    let health_score = ((score + 0.30) / 1.20).clamp(0.0, 1.0);
    let tier = if stats.consecutive_failures >= 5 || stats.success_rate < 0.55 {
        "critical"
    } else if health_score >= 0.72 {
        "healthy"
    } else if health_score >= 0.52 {
        "watch"
    } else if health_score >= 0.35 {
        "degraded"
    } else {
        "critical"
    };
    (tier.to_string(), reasons, health_score)
}

// ---------------------------------------------------------------------------
// Build route-autotune plan (takes `state_root` instead of `cli`)
// ---------------------------------------------------------------------------

pub(crate) fn build_route_autotune_plan(
    state_root: &Path,
    learning_path: &Path,
    report_path: &Path,
    entries: &[RouteHealthEntry],
    summary: &serde_json::Value,
) -> RouteAutotunePlan {
    let total = entries.len() as f64;
    let healthy = summary.get("healthy").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let watch = summary.get("watch").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let degraded = summary
        .get("degraded")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as f64;
    let critical = summary
        .get("critical")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as f64;
    let avg_score = summary
        .get("average_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let unhealthy_ratio = if total > 0.0 {
        (degraded + critical) / total
    } else {
        0.0
    };
    let watch_ratio = if total > 0.0 { watch / total } else { 0.0 };

    let mut reasons = Vec::new();
    if total < 3.0 {
        reasons.push("low_evidence_sample".to_string());
    }
    if critical > 0.0 {
        reasons.push("critical_routes_detected".to_string());
    } else if degraded > 0.0 {
        reasons.push("degraded_routes_detected".to_string());
    } else if watch > 0.0 {
        reasons.push("watch_routes_detected".to_string());
    } else if healthy > 0.0 {
        reasons.push("routes_healthy".to_string());
    } else {
        reasons.push("no_routes_learned".to_string());
    }
    if avg_score < 0.45 {
        reasons.push("average_health_low".to_string());
    } else if avg_score >= 0.75 {
        reasons.push("average_health_high".to_string());
    }

    let confidence = if total >= 12.0 {
        "high"
    } else if total >= 5.0 {
        "medium"
    } else {
        "low"
    };

    let cheap_bias = if critical > 0.0 {
        0.16
    } else if unhealthy_ratio >= 0.50 {
        0.14
    } else if unhealthy_ratio >= 0.25 || watch_ratio > 0.45 {
        0.11
    } else if avg_score >= 0.78 {
        0.06
    } else {
        0.08
    };
    let switch_margin = if critical > 0.0 {
        0.07
    } else if degraded > 0.0 {
        0.05
    } else if watch > 0.0 {
        0.04
    } else {
        0.03
    };
    let alpha = if critical > 0.0 {
        0.35
    } else if degraded > 0.0 {
        0.28
    } else if watch > 0.0 {
        0.24
    } else {
        0.20
    };
    let ttl_secs = if critical > 0.0 {
        5 * 24 * 60 * 60
    } else if degraded > 0.0 {
        6 * 24 * 60 * 60
    } else {
        7 * 24 * 60 * 60
    };
    let half_life_secs = if critical > 0.0 {
        12 * 60 * 60
    } else if degraded > 0.0 {
        18 * 60 * 60
    } else if watch > 0.0 {
        22 * 60 * 60
    } else {
        24 * 60 * 60
    };

    let mut overrides = BTreeMap::new();
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_ALPHA".to_string(),
        format!("{:.3}", clamp_f64(alpha, 0.01, 1.0)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS".to_string(),
        format!("{:.3}", clamp_f64(cheap_bias, -0.50, 0.50)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN".to_string(),
        format!("{:.3}", clamp_f64(switch_margin, 0.0, 0.50)),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_TTL_SECS".to_string(),
        clamp_i64(ttl_secs, 0, 30 * 24 * 60 * 60).to_string(),
    );
    overrides.insert(
        "HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS".to_string(),
        clamp_i64(half_life_secs, 0, 30 * 24 * 60 * 60).to_string(),
    );

    RouteAutotunePlan {
        generated_at: chrono::Utc::now().to_rfc3339(),
        learning_path: learning_path.display().to_string(),
        health_report_path: report_path.display().to_string(),
        env_path: CliStateRoot::from_state_root(state_root)
            .route_autotune_env()
            .display()
            .to_string(),
        summary: summary.clone(),
        confidence: confidence.to_string(),
        reasons,
        overrides,
    }
}

// ---------------------------------------------------------------------------
// Env-file helpers
// ---------------------------------------------------------------------------

pub(crate) fn parse_simple_env_file(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, value)) = body.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        out.insert(key.to_string(), value);
    }
    out
}

pub(crate) fn apply_route_autotune_env_overrides(state_root: &Path) -> Vec<String> {
    let path = CliStateRoot::from_state_root(state_root).route_autotune_env();
    if !path.exists() {
        return Vec::new();
    }
    let parsed = parse_simple_env_file(&path);
    let mut applied = Vec::new();
    for key in ROUTE_AUTOTUNE_ENV_KEYS {
        if std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some()
        {
            continue;
        }
        if let Some(value) = parsed.get(*key) {
            hermes_cli::env_vars::set_var(key, value);
            applied.push((*key).to_string());
        }
    }
    applied
}

// ---------------------------------------------------------------------------
// CLI subcommand handlers
// ---------------------------------------------------------------------------

pub(crate) async fn run_route_learning(
    cli: crate::Cli,
    action: Option<String>,
    json: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let state_root = hermes_state_root(&cli);
    let path = CliStateRoot::from_state_root(&state_root).route_learning_state();
    match action.as_str() {
        "reset" | "clear" => {
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AgentError::Io(format!("remove {}: {}", path.display(), e)))?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "path": path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-learning state cleared: {}", path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" => {}
        _ => {
            return Err(AgentError::Config(format!(
                "route-learning: unsupported action '{}'; use show/list/inspect/reset/clear",
                action
            )));
        }
    }

    let state = load_route_learning_state_for_cli(&path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut rows: Vec<(String, RouteLearningStatsRecord, f64)> = state
        .entries
        .iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(stats, now_ms).map(|effective| {
                (
                    key.clone(),
                    effective.clone(),
                    route_learning_score(&effective),
                )
            })
        })
        .collect();
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    if json {
        let body = serde_json::json!({
            "path": path.display().to_string(),
            "ttl_secs": route_learning_ttl_secs(),
            "half_life_secs": route_learning_half_life_secs(),
            "saved_at_unix_ms": state.saved_at_unix_ms,
            "entries": rows.iter().map(|(key, stats, score)| {
                serde_json::json!({
                    "key": key,
                    "score": score,
                    "stats": stats,
                })
            }).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&body)
                .map_err(|e| AgentError::Config(format!("serialize route-learning json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-learning state: {}", path.display());
    println!(
        "TTL={}s half_life={}s entries={}",
        route_learning_ttl_secs(),
        route_learning_half_life_secs(),
        rows.len()
    );
    if rows.is_empty() {
        println!("(no learned routes yet)");
        return Ok(());
    }
    println!();
    println!(
        "{:<42}  {:>7}  {:>8}  {:>10}  {:>8}  {:>14}",
        "ROUTE", "SCORE", "SUCCESS", "LAT_MS", "FAILURES", "UPDATED_AT_MS"
    );
    for (key, stats, score) in rows {
        println!(
            "{:<42}  {:>7.3}  {:>7.2}%  {:>10.1}  {:>8}  {:>14}",
            key,
            score,
            stats.success_rate * 100.0,
            stats.avg_latency_ms,
            stats.consecutive_failures,
            stats.updated_at_unix_ms
        );
    }
    Ok(())
}

pub(crate) async fn run_route_health(
    cli: crate::Cli,
    action: Option<String>,
    json: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let state_root = hermes_state_root(&cli);
    let report_path = CliStateRoot::from_state_root(&state_root).route_health_state();

    match action.as_str() {
        "reset" | "clear" => {
            if report_path.exists() {
                std::fs::remove_file(&report_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", report_path.display(), e))
                })?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "path": report_path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-health report cleared: {}", report_path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" => {}
        _ => {
            return Err(AgentError::Config(format!(
                "route-health: unsupported action '{}'; use show/list/inspect/reset/clear",
                action
            )));
        }
    }

    let state_root = hermes_state_root(&cli);
    let learning_path = CliStateRoot::from_state_root(&state_root).route_learning_state();
    let state = load_route_learning_state_for_cli(&learning_path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut entries: Vec<RouteHealthEntry> = state
        .entries
        .into_iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(&stats, now_ms).map(|effective| {
                let score = route_learning_score(&effective);
                let (tier, reasons, health_score) = route_health_tier(&effective, score);
                RouteHealthEntry {
                    key,
                    health_score,
                    tier,
                    reasons,
                    stats: effective,
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });

    let healthy = entries.iter().filter(|e| e.tier == "healthy").count();
    let watch = entries.iter().filter(|e| e.tier == "watch").count();
    let degraded = entries.iter().filter(|e| e.tier == "degraded").count();
    let critical = entries.iter().filter(|e| e.tier == "critical").count();
    let overall = if critical > 0 {
        "critical"
    } else if degraded > 0 {
        "degraded"
    } else if watch > 0 {
        "watch"
    } else if healthy > 0 {
        "healthy"
    } else {
        "unknown"
    };
    let avg_score = if entries.is_empty() {
        0.0
    } else {
        entries.iter().map(|e| e.health_score).sum::<f64>() / (entries.len() as f64)
    };

    let payload = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "path": report_path.display().to_string(),
        "learning_path": learning_path.display().to_string(),
        "summary": {
            "entries": entries.len(),
            "overall": overall,
            "average_score": avg_score,
            "healthy": healthy,
            "watch": watch,
            "degraded": degraded,
            "critical": critical,
        },
        "entries": entries,
    });

    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| AgentError::Config(format!("serialize route-health: {}", e)))?;
    std::fs::write(&report_path, body)
        .map_err(|e| AgentError::Io(format!("write {}: {}", report_path.display(), e)))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize route-health json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-health report: {}", report_path.display());
    println!(
        "Overall={} entries={} avg_score={:.3} (healthy={} watch={} degraded={} critical={})",
        overall,
        payload["summary"]["entries"].as_u64().unwrap_or(0),
        avg_score,
        healthy,
        watch,
        degraded,
        critical
    );
    if let Some(items) = payload["entries"].as_array() {
        if items.is_empty() {
            println!("(no routes learned yet)");
            return Ok(());
        }
        println!(
            "{:<42}  {:>7}  {:<9}  {:>8}  {:>10}  {:>8}",
            "ROUTE", "HEALTH", "TIER", "SUCCESS", "LAT_MS", "FAILURES"
        );
        for item in items {
            let key = item["key"].as_str().unwrap_or("");
            let health = item["health_score"].as_f64().unwrap_or(0.0);
            let tier = item["tier"].as_str().unwrap_or("unknown");
            let stats = item["stats"].as_object();
            let success = stats
                .and_then(|s| s.get("success_rate"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let latency = stats
                .and_then(|s| s.get("avg_latency_ms"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let failures = stats
                .and_then(|s| s.get("consecutive_failures"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            println!(
                "{:<42}  {:>7.3}  {:<9}  {:>7.2}%  {:>10.1}  {:>8}",
                key,
                health,
                tier,
                success * 100.0,
                latency,
                failures
            );
        }
    }
    Ok(())
}

pub(crate) async fn run_route_autotune(
    cli: crate::Cli,
    action: Option<String>,
    apply: bool,
    strict: bool,
    json: bool,
) -> Result<(), AgentError> {
    let action = action
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "show".to_string());
    let state_root = hermes_state_root(&cli);
    let route_report_path = CliStateRoot::from_state_root(&state_root).route_health_state();
    let autotune_state_path = CliStateRoot::from_state_root(&state_root).route_autotune_state();
    let autotune_env_path = CliStateRoot::from_state_root(&state_root).route_autotune_env();

    match action.as_str() {
        "reset" | "clear" => {
            if autotune_state_path.exists() {
                std::fs::remove_file(&autotune_state_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", autotune_state_path.display(), e))
                })?;
            }
            if autotune_env_path.exists() {
                std::fs::remove_file(&autotune_env_path).map_err(|e| {
                    AgentError::Io(format!("remove {}: {}", autotune_env_path.display(), e))
                })?;
            }
            let payload = serde_json::json!({
                "ok": true,
                "action": action,
                "state_path": autotune_state_path.display().to_string(),
                "env_path": autotune_env_path.display().to_string(),
            });
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("Route-autotune artifacts cleared.");
                println!("State: {}", autotune_state_path.display());
                println!("Env:   {}", autotune_env_path.display());
            }
            return Ok(());
        }
        "show" | "list" | "inspect" | "plan" | "apply" => {}
        _ => {
            return Err(AgentError::Config(format!(
                "route-autotune: unsupported action '{}'; use show/list/inspect/plan/apply/reset/clear",
                action
            )));
        }
    }

    let state_root = hermes_state_root(&cli);
    let learning_path = CliStateRoot::from_state_root(&state_root).route_learning_state();
    let state = load_route_learning_state_for_cli(&learning_path)?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut entries: Vec<RouteHealthEntry> = state
        .entries
        .into_iter()
        .filter_map(|(key, stats)| {
            route_learning_effective_stats(&stats, now_ms).map(|effective| {
                let score = route_learning_score(&effective);
                let (tier, reasons, health_score) = route_health_tier(&effective, score);
                RouteHealthEntry {
                    key,
                    health_score,
                    tier,
                    reasons,
                    stats: effective,
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });

    let healthy = entries.iter().filter(|e| e.tier == "healthy").count();
    let watch = entries.iter().filter(|e| e.tier == "watch").count();
    let degraded = entries.iter().filter(|e| e.tier == "degraded").count();
    let critical = entries.iter().filter(|e| e.tier == "critical").count();
    let overall = if critical > 0 {
        "critical"
    } else if degraded > 0 {
        "degraded"
    } else if watch > 0 {
        "watch"
    } else if healthy > 0 {
        "healthy"
    } else {
        "unknown"
    };
    let avg_score = if entries.is_empty() {
        0.0
    } else {
        entries.iter().map(|e| e.health_score).sum::<f64>() / (entries.len() as f64)
    };

    let summary = serde_json::json!({
        "entries": entries.len(),
        "overall": overall,
        "average_score": avg_score,
        "healthy": healthy,
        "watch": watch,
        "degraded": degraded,
        "critical": critical,
    });
    let state_root = hermes_state_root(&cli);
    let plan = build_route_autotune_plan(
        &state_root,
        &learning_path,
        &route_report_path,
        &entries,
        &summary,
    );
    if strict && plan.confidence == "low" {
        return Err(AgentError::Config(
            "route-autotune strict mode requires at least 5 learned routes".to_string(),
        ));
    }

    if let Some(parent) = autotune_state_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    std::fs::write(
        &autotune_state_path,
        serde_json::to_string_pretty(&plan)
            .map_err(|e| AgentError::Config(format!("serialize route-autotune plan: {}", e)))?,
    )
    .map_err(|e| AgentError::Io(format!("write {}: {}", autotune_state_path.display(), e)))?;

    let should_apply = apply || action == "apply";
    if should_apply {
        if let Some(parent) = autotune_env_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
        }
        let mut body = String::new();
        body.push_str("# Hermes Agent Ultra route-autotune overrides\n");
        body.push_str(&format!("# generated_at={}\n", plan.generated_at));
        for (key, value) in &plan.overrides {
            body.push_str(&format!("{key}={value}\n"));
        }
        std::fs::write(&autotune_env_path, body)
            .map_err(|e| AgentError::Io(format!("write {}: {}", autotune_env_path.display(), e)))?;
    }

    let payload = serde_json::json!({
        "ok": true,
        "action": action,
        "applied": should_apply,
        "strict": strict,
        "state_path": autotune_state_path.display().to_string(),
        "env_path": autotune_env_path.display().to_string(),
        "plan": plan,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize route-autotune json: {}", e)))?
        );
        return Ok(());
    }

    println!("Route-autotune plan: {}", autotune_state_path.display());
    println!(
        "Overall={} entries={} avg_score={:.3} confidence={} applied={}",
        payload["plan"]["summary"]["overall"]
            .as_str()
            .unwrap_or("unknown"),
        payload["plan"]["summary"]["entries"].as_u64().unwrap_or(0),
        payload["plan"]["summary"]["average_score"]
            .as_f64()
            .unwrap_or(0.0),
        payload["plan"]["confidence"].as_str().unwrap_or("low"),
        if should_apply { "yes" } else { "no" },
    );
    if let Some(reasons) = payload["plan"]["reasons"].as_array() {
        if !reasons.is_empty() {
            println!(
                "Reasons: {}",
                reasons
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    println!("\nSuggested overrides:");
    if let Some(obj) = payload["plan"]["overrides"].as_object() {
        for (key, value) in obj {
            println!("  {:<44} {}", key, value.as_str().unwrap_or(""));
        }
    }
    if should_apply {
        println!(
            "\nApplied overrides file: {} (loaded automatically on next start unless env explicitly overrides a key)",
            autotune_env_path.display()
        );
    } else {
        println!("\nRun `hermes route-autotune apply --apply` to persist these overrides.");
    }
    Ok(())
}

pub(crate) async fn run_incident_pack(
    cli: crate::Cli,
    snapshot: Option<String>,
    output: Option<String>,
    json: bool,
) -> Result<(), AgentError> {
    let snapshot_path = if let Some(path) = snapshot
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        if !path.exists() {
            return Err(AgentError::Config(format!(
                "incident-pack snapshot not found: {}",
                path.display()
            )));
        }
        path
    } else {
        let payload = serde_json::json!({
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "mode": "incident_pack_snapshot",
            "state_root": crate::hermes_state_root(&cli).display().to_string(),
            "elite": build_elite_doctor_diagnostics(&cli),
        });
        let out = write_doctor_snapshot(&cli, &payload, None)?;
        if let Ok(snapshot_bytes) = std::fs::read(&out) {
            if let Ok(sig) =
                sign_artifact_bytes(&crate::hermes_state_root(&cli), &snapshot_bytes, true)
            {
                let _ = write_provenance_sidecar(&out, &sig);
            }
        }
        out
    };

    let output_path = output
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let bundle = build_doctor_support_bundle_with_options(
        &cli,
        &snapshot_path,
        output_path.as_deref(),
        true,
    )?;

    let bundle_sig_path = if let Ok(bundle_bytes) = std::fs::read(&bundle) {
        sign_artifact_bytes(&crate::hermes_state_root(&cli), &bundle_bytes, true)
            .ok()
            .and_then(|sig| write_provenance_sidecar(&bundle, &sig).ok())
            .map(|p| p.display().to_string())
    } else {
        None
    };

    let payload = serde_json::json!({
        "ok": true,
        "deterministic": true,
        "snapshot": snapshot_path.display().to_string(),
        "bundle": bundle.display().to_string(),
        "bundle_signature": bundle_sig_path,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|e| AgentError::Config(format!("serialize incident-pack json: {}", e)))?
        );
    } else {
        println!("Incident pack created: {}", bundle.display());
        println!("Snapshot: {}", snapshot_path.display());
        if let Some(sig) = payload["bundle_signature"].as_str() {
            println!("Bundle signature: {}", sig);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hermes_state_root;
    use clap::Parser;
    use std::path::Path;

    #[test]
    fn route_health_tier_marks_failure_streak_critical() {
        let stats = RouteLearningStatsRecord {
            samples: 8,
            success_rate: 0.61,
            avg_latency_ms: 2200.0,
            consecutive_failures: 6,
            updated_at_unix_ms: 1_700_000_000_000,
        };
        let (tier, reasons, score) = route_health_tier(&stats, route_learning_score(&stats));
        assert_eq!(tier, "critical");
        assert!(reasons.iter().any(|r| r == "failure_streak_critical"));
        assert!(score >= 0.0);
    }

    #[test]
    fn parse_simple_env_file_supports_export_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_path = tmp.path().join("route-autotune.env");
        std::fs::write(
            &env_path,
            "# comment\nexport HERMES_SMART_ROUTING_LEARNING_ALPHA=0.240\nHERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS=0.110\n",
        )
        .expect("write env");
        let parsed = parse_simple_env_file(&env_path);
        assert_eq!(
            parsed
                .get("HERMES_SMART_ROUTING_LEARNING_ALPHA")
                .map(String::as_str),
            Some("0.240")
        );
        assert_eq!(
            parsed
                .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
                .map(String::as_str),
            Some("0.110")
        );
    }

    #[test]
    fn apply_route_autotune_env_overrides_sets_missing_keys_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = crate::Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "status",
        ]);
        let state_root = crate::hermes_state_root(&cli);
        let env_path = CliStateRoot::from_state_root(&state_root).route_autotune_env();
        if let Some(parent) = env_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(
            &env_path,
            "HERMES_SMART_ROUTING_LEARNING_ALPHA=0.300\nHERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN=0.050\n",
        )
        .expect("write env");

        hermes_cli::env_vars::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
        hermes_cli::env_vars::set_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN", "0.999");
        let applied = apply_route_autotune_env_overrides(&state_root);
        assert!(
            applied
                .iter()
                .any(|k| k == "HERMES_SMART_ROUTING_LEARNING_ALPHA")
        );
        assert_eq!(
            std::env::var("HERMES_SMART_ROUTING_LEARNING_ALPHA").ok(),
            Some("0.300".to_string())
        );
        assert_eq!(
            std::env::var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN").ok(),
            Some("0.999".to_string()),
            "explicit env var should not be overridden"
        );
        hermes_cli::env_vars::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
        hermes_cli::env_vars::remove_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN");
    }

    #[test]
    fn build_route_autotune_plan_raises_bias_for_critical_health() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = crate::Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "status",
        ]);
        let state_root = crate::hermes_state_root(&cli);
        let entry = RouteHealthEntry {
            key: "openai:gpt-4o".to_string(),
            health_score: 0.2,
            tier: "critical".to_string(),
            reasons: vec!["failure_streak_critical".to_string()],
            stats: RouteLearningStatsRecord {
                samples: 9,
                success_rate: 0.4,
                avg_latency_ms: 5200.0,
                consecutive_failures: 7,
                updated_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            },
        };
        let summary = serde_json::json!({
            "entries": 1,
            "overall": "critical",
            "average_score": 0.2,
            "healthy": 0,
            "watch": 0,
            "degraded": 0,
            "critical": 1
        });
        let plan = build_route_autotune_plan(
            &state_root,
            Path::new("/tmp/route-learning.json"),
            Path::new("/tmp/route-health.json"),
            &[entry],
            &summary,
        );
        let cheap_bias = plan
            .overrides
            .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let switch_margin = plan
            .overrides
            .get("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        assert!(cheap_bias >= 0.14);
        assert!(switch_margin >= 0.05);
        assert_eq!(plan.confidence, "low");
    }
}
