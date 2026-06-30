async fn run_autopilot_probe_command(
    command: &str,
    cwd: &Path,
    max_tail: usize,
) -> serde_json::Value {
    run_autopilot_probe_command_with_timeout(command, cwd, max_tail, autopilot_probe_timeout())
        .await
}

fn autopilot_probe_timeout() -> Duration {
    std::env::var("HERMES_AUTOPILOT_PROBE_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| (1..=3600).contains(secs))
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(600))
}

async fn run_autopilot_probe_command_with_timeout(
    command: &str,
    cwd: &Path,
    max_tail: usize,
    timeout: Duration,
) -> serde_json::Value {
    let started = chrono::Utc::now();
    let mut child = tokio::process::Command::new("bash");
    child
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .suppress_windows_console()
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, child.output()).await;
    let finished = chrono::Utc::now();
    match output {
        Ok(Ok(output)) => serde_json::json!({
            "command": command,
            "exit_code": output.status.code().unwrap_or(-1),
            "ok": output.status.success(),
            "started_at": started.to_rfc3339(),
            "finished_at": finished.to_rfc3339(),
            "duration_ms": (finished - started).num_milliseconds().max(0),
            "stdout_tail": tail_chars(&String::from_utf8_lossy(&output.stdout), max_tail),
            "stderr_tail": tail_chars(&String::from_utf8_lossy(&output.stderr), max_tail),
        }),
        Ok(Err(err)) => serde_json::json!({
            "command": command,
            "exit_code": -1,
            "ok": false,
            "started_at": started.to_rfc3339(),
            "finished_at": finished.to_rfc3339(),
            "duration_ms": (finished - started).num_milliseconds().max(0),
            "stdout_tail": "",
            "stderr_tail": format!("spawn failed: {err}"),
        }),
        Err(_) => serde_json::json!({
            "command": command,
            "exit_code": -1,
            "ok": false,
            "started_at": started.to_rfc3339(),
            "finished_at": finished.to_rfc3339(),
            "duration_ms": (finished - started).num_milliseconds().max(0),
            "stdout_tail": "",
            "stderr_tail": format!("timed out after {}ms", timeout.as_millis()),
        }),
    }
}

fn autopilot_native_section_from_report(
    command: &str,
    report: &serde_json::Value,
    path: &Path,
) -> serde_json::Value {
    let ok = report.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let stdout_tail = serde_json::to_string_pretty(report).unwrap_or_else(|_| report.to_string());
    serde_json::json!({
        "command": command,
        "exit_code": if ok { 0 } else { 1 },
        "ok": ok,
        "started_at": report.get("generated_at").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "finished_at": report.get("generated_at").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "duration_ms": 0,
        "stdout_tail": format!("{}\nreport_path={}", stdout_tail, path.display()),
        "stderr_tail": "",
    })
}

fn contextlattice_orchestrator_url() -> String {
    std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|_| std::env::var("CONTEXTLATTICE_URL"))
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8075".to_string())
}

async fn contextlattice_preflight_section() -> serde_json::Value {
    let started = chrono::Utc::now();
    let base = contextlattice_orchestrator_url();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            let finished = chrono::Utc::now();
            return serde_json::json!({
                "command": format!("GET {base}/health + POST {base}/memory/search"),
                "exit_code": -1,
                "ok": false,
                "started_at": started.to_rfc3339(),
                "finished_at": finished.to_rfc3339(),
                "duration_ms": (finished - started).num_milliseconds().max(0),
                "stdout_tail": "",
                "stderr_tail": format!("ContextLattice HTTP client build failed: {err}"),
            });
        }
    };

    let health_result = client
        .get(format!("{base}/health"))
        .send()
        .await
        .and_then(|resp| resp.error_for_status());
    let (health_ok, health_json, mut warnings) = match health_result {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(value) => {
                let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                (ok, value, Vec::<String>::new())
            }
            Err(err) => (
                false,
                serde_json::json!({}),
                vec![format!("health_json_parse_failed: {err}")],
            ),
        },
        Err(err) => (
            false,
            serde_json::json!({}),
            vec![format!("health_request_failed: {err}")],
        ),
    };

    let search_payload = serde_json::json!({
        "agent_id": std::env::var("CONTEXTLATTICE_AGENT_ID").unwrap_or_else(|_| "codex_gpt5".to_string()),
        "query": "hermes-ultra contextlattice intelligence preflight",
        "limit": 2,
        "retrieval_mode": "fast",
    });
    let search_result = client
        .post(format!("{base}/memory/search"))
        .json(&search_payload)
        .send()
        .await
        .and_then(|resp| resp.error_for_status());
    let search_json = match search_result {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!("search_json_parse_failed: {err}"));
                serde_json::json!({})
            }
        },
        Err(err) => {
            warnings.push(format!("search_request_failed: {err}"));
            serde_json::json!({})
        }
    };
    let degraded = search_json
        .get("degraded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let retrieval = search_json
        .get("retrieval_debug")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let pending_total = health_json
        .pointer("/telemetry/queueDepth")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let payload = serde_json::json!({
        "health": health_json,
        "warnings": warnings,
        "context_pack": {
            "retrieval": retrieval,
            "result_state": search_json.get("result_state").cloned().unwrap_or(serde_json::Value::Null),
            "degraded": degraded,
        },
        "status": {
            "queue": {
                "pendingTotal": pending_total,
            }
        }
    });
    let stdout_tail =
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    let finished = chrono::Utc::now();
    serde_json::json!({
        "command": format!("GET {base}/health + POST {base}/memory/search"),
        "exit_code": if health_ok && !degraded { 0 } else { 1 },
        "ok": health_ok && !degraded,
        "started_at": started.to_rfc3339(),
        "finished_at": finished.to_rfc3339(),
        "duration_ms": (finished - started).num_milliseconds().max(0),
        "stdout_tail": tail_chars(&stdout_tail, 240000),
        "stderr_tail": "",
    })
}

fn parse_hotpath_ns_from_text(text: &str) -> Option<u64> {
    let needle = "tool_policy_hot_path_ns_per_eval=";
    let idx = text.rfind(needle)?;
    text[idx + needle.len()..]
        .lines()
        .next()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

fn autopilot_section_ok(section: &serde_json::Value) -> bool {
    section.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn autopilot_section_text(section: &serde_json::Value) -> String {
    format!(
        "{}\n{}",
        section
            .get("stdout_tail")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        section
            .get("stderr_tail")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    )
}

fn parse_contextlattice_payload(section: &serde_json::Value) -> Option<serde_json::Value> {
    let raw = section
        .get("stdout_tail")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim();
    if raw.is_empty() {
        return None;
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

fn contextlattice_autopilot_summary(
    payload: Option<&serde_json::Value>,
) -> (bool, usize, i64, String, serde_json::Value, i64) {
    let Some(payload) = payload else {
        return (false, 0, 0, String::new(), serde_json::json!({}), 0);
    };
    let healthy = payload
        .get("health")
        .and_then(|h| h.get("ok"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let warnings = payload
        .get("warnings")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let retrieval = payload
        .pointer("/context_pack/retrieval")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let route_owner_class = retrieval
        .get("route_owner_class")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let source_counts = retrieval
        .get("source_counts")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let python_fallbacks = retrieval
        .pointer("/fallback_counts/python_hot_path_total")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let queue_pending_total = payload
        .pointer("/status/queue/pendingTotal")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    (
        healthy,
        warnings,
        python_fallbacks,
        route_owner_class,
        source_counts,
        queue_pending_total,
    )
}

fn build_performance_autopilot_recommendations(
    hotpath: &serde_json::Value,
    eval_gate: &serde_json::Value,
    mcp_gate: &serde_json::Value,
    context_gate: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut recs = Vec::new();
    let ns = parse_hotpath_ns_from_text(&autopilot_section_text(hotpath));
    let ctx_payload = parse_contextlattice_payload(context_gate);
    let (ctx_healthy, _warnings, python_fallbacks, _route_owner, source_counts, queue_pending) =
        contextlattice_autopilot_summary(ctx_payload.as_ref());

    if !autopilot_section_ok(hotpath) {
        recs.push(serde_json::json!({
            "id": "HOTPATH_FAIL",
            "severity": "P0",
            "title": "Hot-path benchmark failed",
            "recommendation": "Run `cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture` and resolve regressions before release.",
        }));
    } else if ns.is_some_and(|v| v > 12_000) {
        recs.push(serde_json::json!({
            "id": "HOTPATH_SLOW",
            "severity": "P1",
            "title": "Tool policy hot-path latency above target",
            "recommendation": "Keep `HERMES_TOOL_POLICY_PRESET=standard`, review deny-pattern complexity, and rerun the Rust hot-path benchmark.",
        }));
    }
    if !autopilot_section_ok(eval_gate) {
        recs.push(serde_json::json!({
            "id": "EVAL_TREND_FAIL",
            "severity": "P0",
            "title": "Eval trend gate failed",
            "recommendation": "Run `/ops eval run` and address the latest eval trend report before promotion.",
        }));
    }
    if !autopilot_section_ok(mcp_gate) {
        recs.push(serde_json::json!({
            "id": "MCP_STALE_RECOVERY_FAIL",
            "severity": "P1",
            "title": "MCP stale transport recovery regression",
            "recommendation": "Run `cargo test -p hermes-mcp` and restore reconnect-on-stale behavior before promotion.",
        }));
    }
    if !autopilot_section_ok(context_gate) {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_PREFLIGHT_FAIL",
            "severity": "P0",
            "title": "ContextLattice preflight failed",
            "recommendation": "Run `/integrations snapshot` and resolve orchestrator health/retrieval before long objective loops.",
        }));
    } else if !ctx_healthy {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_UNHEALTHY",
            "severity": "P1",
            "title": "ContextLattice health is degraded",
            "recommendation": "Use `/objective context max` and confirm orchestrator health/retrieval lanes before long-running objective loops.",
        }));
    }
    if python_fallbacks > 0 {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_PYTHON_FALLBACK",
            "severity": "P1",
            "title": "ContextLattice retrieval fallback detected",
            "recommendation": "Investigate non-native fallback causes and keep Go/Rust lanes hot to avoid degraded memory-intelligence behavior.",
        }));
    }
    if source_counts.as_object().is_some_and(|m| m.is_empty()) && python_fallbacks == 0 {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
            "severity": "P1",
            "title": "ContextLattice source coverage is empty",
            "recommendation": "Use broader same-project context-pack and ensure topic rollups/primary stores return at least one grounded hit.",
        }));
    }
    if queue_pending > 8 {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_QUEUE_PRESSURE",
            "severity": "P2",
            "title": "ContextLattice queue pressure elevated",
            "recommendation": "Reduce write burst size or raise checkpoint spacing for long loops until pending queue normalizes.",
        }));
    }
    if recs.is_empty() {
        recs.push(serde_json::json!({
            "id": "PERF_STABLE",
            "severity": "P3",
            "title": "Performance checks stable",
            "recommendation": "No immediate tuning required. Keep nightly elite gate cadence.",
        }));
    }
    recs
}

fn recommendation_ids(recommendations: &[serde_json::Value]) -> HashSet<String> {
    recommendations
        .iter()
        .filter_map(|rec| rec.get("id").and_then(|v| v.as_str()))
        .map(|id| id.to_string())
        .collect()
}

fn compute_performance_autopilot_indexes(
    hotpath: &serde_json::Value,
    eval_gate: &serde_json::Value,
    mcp_gate: &serde_json::Value,
    context_gate: &serde_json::Value,
    recommendations: &[serde_json::Value],
) -> serde_json::Value {
    let ns = parse_hotpath_ns_from_text(&autopilot_section_text(hotpath));
    let checks = [
        autopilot_section_ok(hotpath),
        autopilot_section_ok(eval_gate),
        autopilot_section_ok(mcp_gate),
        autopilot_section_ok(context_gate),
    ];
    let fail_count = checks.iter().filter(|ok| !**ok).count();
    let mut performance = 100.0f64;
    if let Some(ns) = ns {
        if ns > 12_000 {
            let overflow_ratio = ((ns - 12_000) as f64 / 12_000.0).min(3.0);
            performance -= (overflow_ratio * 10.0).min(30.0);
        }
    }
    if !autopilot_section_ok(hotpath) {
        performance -= 35.0;
    }
    if !autopilot_section_ok(mcp_gate) {
        performance -= 20.0;
    }
    if !autopilot_section_ok(context_gate) {
        performance -= 25.0;
    }
    performance = performance.clamp(0.0, 100.0);

    let mut intelligence = 100.0f64;
    for rec in recommendations {
        let sev = rec
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("P3")
            .to_ascii_uppercase();
        intelligence -= match sev.as_str() {
            "P0" => 22.0,
            "P1" => 12.0,
            "P2" => 6.0,
            _ => 2.0,
        };
    }
    if !autopilot_section_ok(eval_gate) {
        intelligence -= 18.0;
    }
    if !autopilot_section_ok(context_gate) {
        intelligence -= 20.0;
    }
    let ctx_payload = parse_contextlattice_payload(context_gate);
    let (_healthy, _warnings, python_fallbacks, _route_owner, source_counts, queue_pending) =
        contextlattice_autopilot_summary(ctx_payload.as_ref());
    if python_fallbacks > 0 {
        intelligence -= (python_fallbacks as f64).min(12.0);
    }
    if source_counts.as_object().is_some_and(|m| m.is_empty()) && python_fallbacks == 0 {
        intelligence -= 10.0;
    }
    if queue_pending > 8 {
        intelligence -= 8.0;
    }
    intelligence = intelligence.clamp(0.0, 100.0);

    let profile = if fail_count >= 2 {
        "safety"
    } else if !autopilot_section_ok(eval_gate) || !autopilot_section_ok(context_gate) {
        "quality"
    } else if !autopilot_section_ok(mcp_gate) {
        "reliability"
    } else if ns.is_some_and(|v| v > 12_000) {
        "throughput"
    } else {
        "balanced"
    };
    let mut adaptive_actions = vec![serde_json::json!({
        "key": "HERMES_PERF_AUTOPILOT_PROFILE",
        "value": profile,
        "reason": "profile recommendation from adaptive index",
    })];
    match profile {
        "throughput" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_TOOL_POLICY_PRESET","value":"standard","reason":"reduce policy hot-path overhead"}));
            adaptive_actions.push(serde_json::json!({"key":"HERMES_MODEL_CATALOG_GUARD","value":"1","reason":"avoid invalid model retries"}));
        }
        "quality" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_REPLAY_ENABLED","value":"1","reason":"capture deterministic replay for eval failures"}));
            adaptive_actions.push(serde_json::json!({"key":"HERMES_MODEL_AUTO_REMEDIATE","value":"1","reason":"promote self-heal recommendation loop"}));
        }
        "reliability" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_TOOL_POLICY_MODE","value":"enforce","reason":"stabilize stale transport/recovery behavior"}));
        }
        "safety" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_TOOL_POLICY_MODE","value":"enforce","reason":"strict policy posture under multi-check failure"}));
            adaptive_actions.push(serde_json::json!({"key":"HERMES_REPLAY_ENABLED","value":"1","reason":"preserve incident evidence during degraded state"}));
        }
        _ => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_PERF_AUTOPILOT_STATUS","value":"stable","reason":"all checks stable"}));
        }
    }
    serde_json::json!({
        "performance_index": (performance * 100.0).round() / 100.0,
        "intelligence_index": (intelligence * 100.0).round() / 100.0,
        "adaptive_index": ((performance * 0.55 + intelligence * 0.45) * 100.0).round() / 100.0,
        "profile_recommendation": profile,
        "adaptive_actions": adaptive_actions,
    })
}

fn default_performance_autopilot_paths(repo_root: &Path) -> (PathBuf, PathBuf) {
    let stamp = utc_compact_stamp();
    let out_dir = repo_root.join(".sync-reports");
    (
        out_dir.join(format!("performance-autopilot-{stamp}.json")),
        out_dir.join(format!("performance-autopilot-{stamp}.md")),
    )
}

fn write_performance_autopilot_markdown(
    path: &Path,
    report: &serde_json::Value,
) -> Result<(), AgentError> {
    let mut lines = Vec::new();
    lines.push("# Performance Autopilot Report".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- generated_at: `{}`",
        report
            .get("generated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    ));
    lines.push(format!(
        "- ok: `{}`",
        report.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
    ));
    lines.push(format!(
        "- intelligence_index: `{:.2}`",
        report
            .get("intelligence_index")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    ));
    lines.push(format!(
        "- performance_index: `{:.2}`",
        report
            .get("performance_index")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    ));
    lines.push(format!(
        "- adaptive_index: `{:.2}`",
        report
            .get("adaptive_index")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    ));
    lines.push(format!(
        "- profile_recommendation: `{}`",
        report
            .get("profile_recommendation")
            .and_then(|v| v.as_str())
            .unwrap_or("balanced")
    ));
    lines.push(String::new());
    lines.push("## Sections".to_string());
    if let Some(sections) = report.get("sections").and_then(|v| v.as_object()) {
        for (name, section) in sections {
            lines.push(format!(
                "- `{}`: {} (exit={})",
                name,
                if autopilot_section_ok(section) {
                    "PASS"
                } else {
                    "FAIL"
                },
                section
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1)
            ));
        }
    }
    lines.push(String::new());
    lines.push("## Recommendations".to_string());
    if let Some(recs) = report.get("recommendations").and_then(|v| v.as_array()) {
        for rec in recs {
            lines.push(format!(
                "- **{} ({})**: {} - {}",
                rec.get("id").and_then(|v| v.as_str()).unwrap_or("UNKNOWN"),
                rec.get("severity").and_then(|v| v.as_str()).unwrap_or("PX"),
                rec.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                rec.get("recommendation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            ));
        }
    }
    if let Some(actions) = report.get("adaptive_actions").and_then(|v| v.as_array()) {
        if !actions.is_empty() {
            lines.push(String::new());
            lines.push("## Adaptive Actions".to_string());
            for action in actions {
                lines.push(format!(
                    "- `{}={}` ({})",
                    action.get("key").and_then(|v| v.as_str()).unwrap_or(""),
                    action.get("value").and_then(|v| v.as_str()).unwrap_or(""),
                    action.get("reason").and_then(|v| v.as_str()).unwrap_or("")
                ));
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create {}: {}", parent.display(), e)))?;
    }
    std::fs::write(path, format!("{}\n", lines.join("\n").trim()))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn write_performance_autopilot_env(
    path: &Path,
    report: &serde_json::Value,
) -> Result<(), AgentError> {
    let recs: Vec<serde_json::Value> = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let rec_ids = recommendation_ids(&recs);
    let mut lines = vec![format!(
        "# generated_at={}",
        report
            .get("generated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    )];
    if rec_ids.contains("HOTPATH_SLOW") {
        lines.extend([
            "HERMES_TOOL_POLICY_PRESET=standard".to_string(),
            "HERMES_TOOL_POLICY_MODE=enforce".to_string(),
            "HERMES_MODEL_CATALOG_GUARD=1".to_string(),
        ]);
    }
    if rec_ids.contains("EVAL_TREND_FAIL") {
        lines.extend([
            "HERMES_MODEL_AUTO_REMEDIATE=1".to_string(),
            "HERMES_REPLAY_ENABLED=1".to_string(),
        ]);
    }
    if rec_ids.iter().any(|id| {
        matches!(
            id.as_str(),
            "CONTEXTLATTICE_PREFLIGHT_FAIL"
                | "CONTEXTLATTICE_UNHEALTHY"
                | "CONTEXTLATTICE_PYTHON_FALLBACK"
                | "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE"
        )
    }) {
        lines.extend([
            "HERMES_CONTEXTLATTICE_MODE=max".to_string(),
            "HERMES_CONTEXTLATTICE_RETRIEVAL_MODE=deep".to_string(),
            "HERMES_CONTEXTLATTICE_REQUIRE_READBACK=1".to_string(),
        ]);
    }
    if rec_ids.len() == 1 && rec_ids.contains("PERF_STABLE") {
        lines.push("HERMES_PERF_AUTOPILOT_STATUS=stable".to_string());
    }
    lines.push(format!(
        "HERMES_PERF_AUTOPILOT_PROFILE={}",
        report
            .get("profile_recommendation")
            .and_then(|v| v.as_str())
            .unwrap_or("balanced")
    ));
    if let Some(actions) = report.get("adaptive_actions").and_then(|v| v.as_array()) {
        for action in actions {
            let Some(key) = action.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(value) = action.get("value").and_then(|v| v.as_str()) else {
                continue;
            };
            lines.push(format!("{key}={value}"));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create {}: {}", parent.display(), e)))?;
    }
    std::fs::write(path, format!("{}\n", lines.join("\n")))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

async fn run_performance_autopilot_native(
    repo_root: &Path,
    apply_env: Option<&Path>,
) -> Result<(serde_json::Value, PathBuf, PathBuf), AgentError> {
    let (json_path, md_path) = default_performance_autopilot_paths(repo_root);
    let hotpath = run_autopilot_probe_command(
        "cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture",
        repo_root,
        6000,
    )
    .await;
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
    let eval_gate =
        autopilot_native_section_from_report("native eval trend gate", &eval_report, &eval_path);
    let mcp_gate = run_autopilot_probe_command(
        "cargo test -p hermes-mcp stale_transport_marker_detection_matches_known_variants -- --nocapture",
        repo_root,
        6000,
    )
    .await;
    let context_gate = contextlattice_preflight_section().await;
    let recommendations =
        build_performance_autopilot_recommendations(&hotpath, &eval_gate, &mcp_gate, &context_gate);
    let ok = [&hotpath, &eval_gate, &mcp_gate, &context_gate]
        .iter()
        .all(|section| autopilot_section_ok(section));
    let adaptive = compute_performance_autopilot_indexes(
        &hotpath,
        &eval_gate,
        &mcp_gate,
        &context_gate,
        &recommendations,
    );
    let mut report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "ok": ok,
        "sections": {
            "hotpath": hotpath,
            "eval_trend": eval_gate,
            "mcp_stale_recovery": mcp_gate,
            "contextlattice_preflight": context_gate,
        },
        "recommendations": recommendations,
        "performance_index": adaptive.get("performance_index").cloned().unwrap_or(serde_json::json!(0.0)),
        "intelligence_index": adaptive.get("intelligence_index").cloned().unwrap_or(serde_json::json!(0.0)),
        "adaptive_index": adaptive.get("adaptive_index").cloned().unwrap_or(serde_json::json!(0.0)),
        "profile_recommendation": adaptive.get("profile_recommendation").cloned().unwrap_or(serde_json::json!("balanced")),
        "adaptive_actions": adaptive.get("adaptive_actions").cloned().unwrap_or_else(|| serde_json::json!([])),
        "report_json": json_path,
        "report_markdown": md_path,
    });
    if let Some(env_path) = apply_env {
        write_performance_autopilot_env(env_path, &report)?;
        report["applied_env"] = serde_json::json!(env_path);
    }
    write_json_report(&json_path, &report)?;
    write_performance_autopilot_markdown(&md_path, &report)?;
    Ok((report, json_path, md_path))
}
