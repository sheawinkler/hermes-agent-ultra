fn report_path_with_stamp(repo_root: &Path, prefix: &str) -> PathBuf {
    repo_root
        .join(".sync-reports")
        .join(format!("{prefix}-{}.json", utc_compact_stamp()))
}

fn gate_section_from_report(
    command: &str,
    report: &serde_json::Value,
    path: &Path,
) -> serde_json::Value {
    autopilot_native_section_from_report(command, report, path)
}

fn parity_release_gate_section(repo_root: &Path) -> serde_json::Value {
    let path = repo_root.join("docs/parity/global-parity-proof.json");
    let report = read_json_file(&path).unwrap_or_else(|| serde_json::json!({}));
    let ok = report
        .pointer("/release_gate/pass")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    serde_json::json!({
        "command": "read docs/parity/global-parity-proof.json release_gate.pass",
        "exit_code": if ok { 0 } else { 1 },
        "ok": ok,
        "elapsed_ms": 0,
        "stdout_tail": serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string()),
        "stderr_tail": if path.exists() { "" } else { "global parity proof missing" },
        "report_path": path,
    })
}

fn shared_backlog_gate_section(repo_root: &Path) -> serde_json::Value {
    let path = repo_root.join("docs/parity/shared-diff-backlog.json");
    let report = read_json_file(&path).unwrap_or_else(|| serde_json::json!({}));
    let pending_classification = report
        .pointer("/summary/pending_classification")
        .and_then(|v| v.as_i64())
        .unwrap_or(i64::MAX);
    let pending_review = report
        .pointer("/summary/pending_review")
        .and_then(|v| v.as_i64())
        .unwrap_or(i64::MAX);
    let ok = pending_classification == 0 && pending_review == 0;
    serde_json::json!({
        "command": "read docs/parity/shared-diff-backlog.json summary pending counts",
        "exit_code": if ok { 0 } else { 1 },
        "ok": ok,
        "elapsed_ms": 0,
        "stdout_tail": serde_json::to_string_pretty(&report.get("summary").cloned().unwrap_or(serde_json::Value::Null)).unwrap_or_default(),
        "stderr_tail": if path.exists() { "" } else { "shared diff backlog missing" },
        "report_path": path,
    })
}

async fn run_slo_auto_rollback_native(
    repo_root: &Path,
    check_cmd: &str,
    rollback_cmd: &str,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let path = report_path
        .map(PathBuf::from)
        .unwrap_or_else(|| report_path_with_stamp(repo_root, "slo-auto-rollback"));
    let check = run_autopilot_probe_command(check_cmd, repo_root, 4000).await;
    let violated = !autopilot_section_ok(&check);
    let rollback = if violated && dry_run {
        serde_json::json!({
            "command": rollback_cmd,
            "ok": false,
            "skipped": true,
            "reason": "dry_run",
        })
    } else if violated {
        run_autopilot_probe_command(rollback_cmd, repo_root, 4000).await
    } else {
        serde_json::Value::Null
    };
    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "ok": !violated,
        "violated": violated,
        "dry_run": dry_run,
        "check": check,
        "rollback": rollback,
        "report_path": path,
    });
    write_json_report(&path, &report)?;
    Ok((report, path))
}

pub async fn run_elite_sync_gate_native(
    repo_root: &Path,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let path = report_path_with_stamp(repo_root, "elite-sync-gate");
    let runtime_python_guard =
        run_autopilot_probe_command("scripts/check-rust-runtime-no-python.sh", repo_root, 4000)
            .await;
    let placeholder_guard =
        run_autopilot_probe_command("scripts/check-runtime-placeholders.sh", repo_root, 4000).await;
    let hotpath = run_autopilot_probe_command(
        "cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture",
        repo_root,
        4000,
    )
    .await;
    let mcp_stale_recovery = run_autopilot_probe_command(
        "cargo test -p hermes-mcp stale_transport_marker_detection_matches_known_variants -- --nocapture",
        repo_root,
        4000,
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
    let eval_trend = gate_section_from_report("native eval trend gate", &eval_report, &eval_path);
    let (autopilot_report, autopilot_json, _autopilot_md) =
        run_performance_autopilot_native(repo_root, None).await?;
    let performance_autopilot = gate_section_from_report(
        "native performance autopilot",
        &autopilot_report,
        &autopilot_json,
    );
    let parity_release = parity_release_gate_section(repo_root);
    let shared_backlog = shared_backlog_gate_section(repo_root);
    let sections = serde_json::json!({
        "runtime_python_guard": runtime_python_guard,
        "placeholder_guard": placeholder_guard,
        "hotpath": hotpath,
        "mcp_stale_recovery": mcp_stale_recovery,
        "eval_trend": eval_trend,
        "performance_autopilot": performance_autopilot,
        "parity_release": parity_release,
        "shared_backlog": shared_backlog,
    });
    let section_values: Vec<&serde_json::Value> = sections
        .as_object()
        .map(|m| m.values().collect())
        .unwrap_or_default();
    let passed = section_values
        .iter()
        .filter(|section| autopilot_section_ok(section))
        .count();
    let total = section_values.len();
    let ok = total > 0 && passed == total;
    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "ok": ok,
        "summary": {
            "total_sections": total,
            "passed_sections": passed,
            "failed_sections": total.saturating_sub(passed),
        },
        "sections": sections,
        "rollback": serde_json::Value::Null,
        "report_path": path,
    });
    write_json_report(&path, &report)?;
    Ok((report, path))
}

fn self_evolution_recommendation(
    rec_id: &str,
    severity: &str,
    title: &str,
    reason: &str,
    command: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": rec_id,
        "severity": severity,
        "title": title,
        "reason": reason,
        "command": command,
    })
}

fn build_self_evolution_recommendations_native(
    objective: &str,
    sections: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let objective_hint = if objective.trim().is_empty() {
        String::new()
    } else {
        format!(" Objective: {}.", objective.trim())
    };
    let section_ok = |name: &str| {
        sections
            .get(name)
            .and_then(|v| v.get("ok"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    if !section_ok("golden_parity") {
        out.push(self_evolution_recommendation(
            "PARITY_DRIFT",
            "P0",
            "Resolve command/TUI parity drift before feature work",
            &format!(
                "Native parity proof or shared backlog gate failed.{}",
                objective_hint
            ),
            "/ops gate status && hermes-ultra doctor --deep --snapshot",
        ));
    }
    if !section_ok("eval_trend") {
        out.push(self_evolution_recommendation(
            "EVAL_REGRESSION",
            "P0",
            "Recover eval trend before promotion",
            &format!("Eval trend gate failed.{}", objective_hint),
            "/ops eval run && /qos autotune plan",
        ));
    }
    if !section_ok("elite_sync") {
        out.push(self_evolution_recommendation(
            "ELITE_GATE_FAIL",
            "P0",
            "Hold release and remediate elite gate failures",
            &format!("Native elite gate failed.{}", objective_hint),
            "/ops gate elite",
        ));
    }
    if out.is_empty() {
        out.push(self_evolution_recommendation(
            "PROMOTE_BASELINE",
            "P2",
            "Promote current state as next baseline",
            &format!(
                "All enabled native sections passed; safe to store this run as a quality baseline.{}",
                objective_hint
            ),
            "hermes-ultra doctor --deep --snapshot",
        ));
    }
    out
}

include!("evolve_and_ops.rs");
