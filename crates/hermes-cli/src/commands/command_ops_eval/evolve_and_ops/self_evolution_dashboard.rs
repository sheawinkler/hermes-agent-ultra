async fn run_self_evolution_loop_native(
    repo_root: &Path,
    objective: &str,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let path = report_path_with_stamp(repo_root, "self-evolution-loop");
    let parity_release = parity_release_gate_section(repo_root);
    let shared_backlog = shared_backlog_gate_section(repo_root);
    let golden_ok = autopilot_section_ok(&parity_release) && autopilot_section_ok(&shared_backlog);
    let golden_parity = serde_json::json!({
        "command": "native parity release/backlog gates",
        "exit_code": if golden_ok { 0 } else { 1 },
        "ok": golden_ok,
        "elapsed_ms": 0,
        "stdout_tail": serde_json::json!({
            "parity_release": parity_release,
            "shared_backlog": shared_backlog,
        }).to_string(),
        "stderr_tail": "",
    });
    let (eval_report, eval_path) = run_eval_trend_gate_native(
        repo_root,
        None,
        None,
        None,
        EvalTrendGateOptions {
            allow_missing_baseline: true,
            ..Default::default()
        },
    )?;
    let eval_trend = gate_section_from_report("native eval trend gate", &eval_report, &eval_path);
    let (elite_report, elite_path) = run_elite_sync_gate_native(repo_root).await?;
    let elite_sync = gate_section_from_report("native elite sync gate", &elite_report, &elite_path);
    let sections = serde_json::json!({
        "golden_parity": golden_parity,
        "eval_trend": eval_trend,
        "elite_sync": elite_sync,
    });
    let section_values: Vec<&serde_json::Value> = sections
        .as_object()
        .map(|m| m.values().collect())
        .unwrap_or_default();
    let total = section_values.len();
    let passed = section_values
        .iter()
        .filter(|section| autopilot_section_ok(section))
        .count();
    let ok = total == 0 || passed == total;
    let recommendations = build_self_evolution_recommendations_native(objective, &sections);
    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "objective": objective.trim(),
        "ok": ok,
        "summary": {
            "total_sections": total,
            "passed_sections": passed,
            "failed_sections": total.saturating_sub(passed),
            "intelligence_index": if total == 0 { 100.0 } else { ((passed as f64 / total as f64) * 10000.0).round() / 100.0 },
        },
        "sections": sections,
        "recommendations": recommendations,
        "report_path": path,
    });
    write_json_report(&path, &report)?;
    Ok((report, path))
}

fn summarize_self_evolution_report(path: &Path, key: &str) -> Option<String> {
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

fn self_evolution_recommendations(path: &Path) -> Vec<String> {
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

const AUTOPILOT_ALLOWED_ENV_KEYS: &[&str] = &[
    "HERMES_TOOL_POLICY_PRESET",
    "HERMES_TOOL_POLICY_MODE",
    "HERMES_MODEL_CATALOG_GUARD",
    "HERMES_MODEL_AUTO_REMEDIATE",
    "HERMES_REPLAY_ENABLED",
    "HERMES_PERF_AUTOPILOT_STATUS",
    "HERMES_PERF_AUTOPILOT_PROFILE",
    "HERMES_PERF_AUTOPILOT_MODE",
];

fn summarize_performance_autopilot_report(path: &Path, key: &str) -> Option<String> {
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

fn performance_autopilot_recommendations(path: &Path) -> Vec<String> {
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

fn parse_env_file_kv(path: &Path) -> Vec<(String, String)> {
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let (k, v) = trimmed.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn write_autopilot_runtime_event(
    report_dir: &Path,
    session_id: &str,
    mode: &str,
    profile: &str,
    applied: &[(String, String)],
) {
    let path = report_dir.join("performance-autopilot-runtime.jsonl");
    let created_at = format!("{:?}", SystemTime::now());
    let payload = serde_json::json!({
        "created_at": created_at,
        "session_id": session_id,
        "mode": mode,
        "profile": profile,
        "applied": applied,
    });
    if let Ok(line) = serde_json::to_string(&payload) {
        if let Ok(mut fh) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(&mut fh, "{line}");
        }
    }
}

fn dashboard_status_line_from_payload(payload: &serde_json::Value) -> String {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("n/a");
    format!(
        "dashboard: {} ({})",
        if enabled { "ON" } else { "OFF" },
        url
    )
}
