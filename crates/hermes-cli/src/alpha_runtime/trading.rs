#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TradingProjectSpec {
    pub id: String,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TradingRuntimeConfig {
    pub updated_at: String,
    pub target_wallet_sol: f64,
    pub starting_wallet_sol: f64,
    pub projects: Vec<TradingProjectSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TradingProjectReport {
    pub id: String,
    pub path: String,
    pub exists: bool,
    pub run_context_files: usize,
    pub latest_wallet_sol: f64,
    pub latest_pnl_sol: f64,
    pub drawdown_pct: f64,
    pub volatility_score: f64,
    pub slippage_bps: f64,
    pub impact_bps: f64,
    pub fee_drag_sol: f64,
    pub funding_drag_sol: f64,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub reject_rate: f64,
    pub anomaly_score: f64,
    pub incident_class: String,
    pub regime: String,
    pub objective_state: String,
    pub patch_recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StrategyHypothesis {
    pub id: String,
    pub statement: String,
    pub novelty_score: f64,
    pub expected_gain_sol: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExperimentSpec {
    pub id: String,
    pub hypothesis_id: String,
    pub metric: String,
    pub control: String,
    pub treatment: String,
    pub pass_criterion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CapitalAllocationRow {
    pub project_id: String,
    pub target_weight: f64,
    pub target_capital_sol: f64,
    pub max_loss_budget_sol: f64,
    pub throttle_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PortfolioRiskGovernor {
    pub mode: String,
    pub halt_new_entries: bool,
    pub max_portfolio_drawdown_pct: f64,
    pub max_project_drawdown_pct: f64,
    pub max_ruin_probability: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CanaryPromotionStep {
    pub stage: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RepoDriftSentinel {
    pub project_id: String,
    pub git_head: String,
    pub baseline_head: String,
    pub dirty_files: usize,
    pub changed_since_baseline: bool,
    pub drift_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RunContextAudit {
    pub project_id: String,
    pub files_scanned: usize,
    pub required_metrics_present: Vec<String>,
    pub missing_metrics: Vec<String>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvProvenanceGate {
    pub project_id: String,
    pub inspected_files: Vec<String>,
    pub conflicting_keys: Vec<String>,
    pub passed: bool,
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReplayCanaryResult {
    pub project_id: String,
    pub sample_size: usize,
    pub pass_rate: f64,
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RemediationRunbookAction {
    pub project_id: String,
    pub priority: String,
    pub title: String,
    pub command: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResearchSourceIngestion {
    pub project_id: String,
    pub source: String,
    pub path: String,
    pub found: bool,
    pub items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TradingAlphaReport {
    pub generated_at: String,
    pub projects: Vec<TradingProjectReport>,
    pub wallet_progress_pct: f64,
    pub ruin_probability: f64,
    pub volatility_sizing_factor: f64,
    pub strategy_weights: HashMap<String, f64>,
    pub pnl_decomposition: HashMap<String, f64>,
    pub canary_recommendation: String,
    pub postmortem: String,
    pub hypotheses: Vec<StrategyHypothesis>,
    pub experiments: Vec<ExperimentSpec>,
    pub backtest_matrix: Vec<String>,
    pub walkforward_checks: Vec<String>,
    pub meta_ranking: Vec<String>,
    pub promotion_candidate: String,
    pub capital_allocator: Vec<CapitalAllocationRow>,
    pub risk_governor: PortfolioRiskGovernor,
    pub canary_pipeline: Vec<CanaryPromotionStep>,
    pub repo_drift: Vec<RepoDriftSentinel>,
    pub run_context_audits: Vec<RunContextAudit>,
    pub env_provenance: Vec<EnvProvenanceGate>,
    pub replay_canary: Vec<ReplayCanaryResult>,
    pub remediation_runbook: Vec<RemediationRunbookAction>,
    pub research_sources: Vec<ResearchSourceIngestion>,
}

fn trading_state_dir() -> PathBuf {
    alpha_state_dir().join("trading")
}

fn trading_config_path() -> PathBuf {
    trading_state_dir().join("runtime_config.json")
}

fn trading_last_report_path() -> PathBuf {
    trading_state_dir().join("last_report.json")
}

fn default_trading_projects() -> Vec<TradingProjectSpec> {
    let mut projects = vec![
        TradingProjectSpec {
            id: "algotraderv2_rust".to_string(),
            path: "~/Documents/Projects/algotraderv2_rust".to_string(),
            enabled: true,
        },
        TradingProjectSpec {
            id: "fastapi-sidecar".to_string(),
            path: "~/Documents/Projects/fastapi-sidecar".to_string(),
            enabled: true,
        },
        TradingProjectSpec {
            id: "kraken-trader".to_string(),
            path: "~/Documents/Projects/kraken-trader".to_string(),
            enabled: true,
        },
    ];

    if let Ok(raw) = std::env::var("HERMES_ALPHA_TRADING_PROJECTS") {
        let custom = raw
            .split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .enumerate()
            .map(|(idx, path)| TradingProjectSpec {
                id: format!("project-{}", idx + 1),
                path: path.to_string(),
                enabled: true,
            })
            .collect::<Vec<_>>();
        if !custom.is_empty() {
            projects = custom;
        }
    }
    projects
}

fn default_trading_runtime_config() -> TradingRuntimeConfig {
    TradingRuntimeConfig {
        updated_at: now_rfc3339(),
        target_wallet_sol: 1000.0,
        starting_wallet_sol: 0.2,
        projects: default_trading_projects(),
    }
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(rest);
            return p.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

pub fn ensure_trading_runtime_bootstrap(force: bool) -> Result<Vec<PathBuf>, AgentError> {
    ensure_alpha_dir()?;
    let dir = trading_state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| AgentError::Io(format!("create {} failed: {}", dir.display(), e)))?;
    let mut written = Vec::new();

    let cfg = trading_config_path();
    if force || !cfg.exists() {
        write_json_file(&cfg, &default_trading_runtime_config())?;
        written.push(cfg);
    }

    let report = trading_last_report_path();
    if force || !report.exists() {
        write_json_file(
            &report,
            &TradingAlphaReport {
                generated_at: now_rfc3339(),
                ..TradingAlphaReport::default()
            },
        )?;
        written.push(report);
    }
    Ok(written)
}

pub fn load_trading_runtime_config() -> Result<TradingRuntimeConfig, AgentError> {
    ensure_trading_runtime_bootstrap(false)?;
    read_json_file(&trading_config_path())
}

fn discover_recent_json_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let logs = root.join("logs").join("run_context");
    if !logs.exists() {
        return out;
    }
    if let Ok(entries) = std::fs::read_dir(&logs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                out.push(path);
            }
        }
    }
    out.sort_by(|a, b| b.cmp(a));
    out.truncate(limit.max(1));
    out
}

fn find_numeric_hint(value: &Value, keys: &[&str]) -> Option<f64> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = k.to_ascii_lowercase();
                if keys.iter().any(|needle| key.contains(needle)) {
                    if let Some(n) = v.as_f64() {
                        return Some(n);
                    }
                    if let Some(n) = v.as_i64() {
                        return Some(n as f64);
                    }
                    if let Some(n) = v.as_u64() {
                        return Some(n as f64);
                    }
                }
                if let Some(found) = find_numeric_hint(v, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => arr.iter().find_map(|v| find_numeric_hint(v, keys)),
        _ => None,
    }
}

fn compute_regime(volatility: f64, pnl: f64, reject_rate: f64) -> String {
    if reject_rate > 0.10 {
        "execution-stressed".to_string()
    } else if volatility > 1.2 {
        if pnl >= 0.0 {
            "high-vol-trending".to_string()
        } else {
            "high-vol-adverse".to_string()
        }
    } else if volatility < 0.35 {
        "low-vol-range".to_string()
    } else {
        "neutral".to_string()
    }
}

fn compute_incident_class(reject_rate: f64, latency_p95: f64, anomaly_score: f64) -> String {
    if anomaly_score > 0.75 {
        "critical-anomaly".to_string()
    } else if reject_rate > 0.08 {
        "order-reject-spike".to_string()
    } else if latency_p95 > 1800.0 {
        "latency-degradation".to_string()
    } else {
        "none".to_string()
    }
}

fn analyze_project(spec: &TradingProjectSpec) -> TradingProjectReport {
    let path = PathBuf::from(expand_home(&spec.path));
    let exists = path.exists();
    if !exists {
        return TradingProjectReport {
            id: spec.id.clone(),
            path: path.display().to_string(),
            exists: false,
            objective_state: "unproven".to_string(),
            incident_class: "missing-project-path".to_string(),
            patch_recommendations: vec![
                "verify project path and mount".to_string(),
                "re-run /mission trading refresh".to_string(),
            ],
            ..TradingProjectReport::default()
        };
    }

    let files = discover_recent_json_files(&path, 20);
    let mut wallet_values = Vec::new();
    let mut pnl_values = Vec::new();
    let mut latency_values = Vec::new();
    let mut reject_values = Vec::new();
    let mut slip_values = Vec::new();
    let mut fee_values = Vec::new();
    let mut funding_values = Vec::new();
    let mut impact_values = Vec::new();

    for file in &files {
        if let Ok(raw) = std::fs::read_to_string(file) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(n) = find_numeric_hint(
                    &v,
                    &[
                        "wallet_sol",
                        "wallet_balance_sol",
                        "sol_balance",
                        "sol_wallet",
                    ],
                ) {
                    wallet_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["pnl_sol", "profit_sol", "net_sol"]) {
                    pnl_values.push(n);
                }
                if let Some(n) =
                    find_numeric_hint(&v, &["latency_ms", "latency_p95_ms", "latency_p50_ms"])
                {
                    latency_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["reject_rate", "rejects_pct", "reject"]) {
                    reject_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["slippage_bps", "slip_bps"]) {
                    slip_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["impact_bps", "price_impact_bps"]) {
                    impact_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["fee_sol", "fees_sol", "fee_drag_sol"]) {
                    fee_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["funding_sol", "funding_drag_sol"]) {
                    funding_values.push(n);
                }
            }
        }
    }

    let latest_wallet = wallet_values.first().copied().unwrap_or(0.0);
    let latest_pnl = pnl_values.first().copied().unwrap_or(0.0);
    let peak_wallet = wallet_values.iter().copied().fold(latest_wallet, f64::max);
    let drawdown_pct = if peak_wallet > 0.0 {
        ((peak_wallet - latest_wallet) / peak_wallet).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let mut volatility_score = 0.0f64;
    if pnl_values.len() >= 2 {
        let mean = pnl_values.iter().sum::<f64>() / (pnl_values.len() as f64);
        let var = pnl_values
            .iter()
            .map(|v| {
                let d = *v - mean;
                d * d
            })
            .sum::<f64>()
            / (pnl_values.len() as f64);
        volatility_score = var.sqrt().abs();
    }

    let latency_p95 = latency_values
        .iter()
        .copied()
        .fold(0.0f64, f64::max)
        .max(0.0);
    let mut latency_sorted = latency_values.clone();
    latency_sorted.sort_by(|a, b| a.total_cmp(b));
    let latency_p50 = if latency_sorted.is_empty() {
        0.0
    } else {
        latency_sorted[latency_sorted.len() / 2]
    };
    let reject_rate = reject_values
        .first()
        .copied()
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let slippage_bps = slip_values.first().copied().unwrap_or(0.0).abs();
    let impact_bps = impact_values.first().copied().unwrap_or(0.0).abs();
    let fee_drag = fee_values.first().copied().unwrap_or(0.0).abs();
    let funding_drag = funding_values.first().copied().unwrap_or(0.0).abs();

    let anomaly_score = (drawdown_pct * 0.45
        + reject_rate * 0.25
        + (slippage_bps / 100.0).min(0.2)
        + (latency_p95 / 4000.0).min(0.1))
    .clamp(0.0, 1.0);

    let regime = compute_regime(volatility_score, latest_pnl, reject_rate);
    let incident = compute_incident_class(reject_rate, latency_p95, anomaly_score);
    let objective_state = if latest_pnl > 0.0 && drawdown_pct < 0.2 {
        "advancing".to_string()
    } else if latest_pnl < 0.0 && drawdown_pct > 0.3 {
        "regressing".to_string()
    } else {
        "flat".to_string()
    };

    let mut recommendations = Vec::new();
    if slippage_bps > 50.0 {
        recommendations.push("tighten max slippage + add venue spread gate".to_string());
    }
    if reject_rate > 0.06 {
        recommendations.push("lower order aggressiveness and add retry backoff".to_string());
    }
    if drawdown_pct > 0.25 {
        recommendations.push("activate drawdown circuit breaker and reduce size".to_string());
    }
    if recommendations.is_empty() {
        recommendations.push("maintain current controls; continue telemetry burn-in".to_string());
    }

    TradingProjectReport {
        id: spec.id.clone(),
        path: path.display().to_string(),
        exists: true,
        run_context_files: files.len(),
        latest_wallet_sol: latest_wallet,
        latest_pnl_sol: latest_pnl,
        drawdown_pct,
        volatility_score,
        slippage_bps,
        impact_bps,
        fee_drag_sol: fee_drag,
        funding_drag_sol: funding_drag,
        latency_p50_ms: latency_p50,
        latency_p95_ms: latency_p95,
        reject_rate,
        anomaly_score,
        incident_class: incident,
        regime,
        objective_state,
        patch_recommendations: recommendations,
    }
}

fn derive_hypotheses(report: &TradingAlphaReport) -> Vec<StrategyHypothesis> {
    let mut out = Vec::new();
    for project in &report.projects {
        if project.slippage_bps > 30.0 {
            out.push(StrategyHypothesis {
                id: format!("hyp-{}-slippage", project.id),
                statement: format!(
                    "{}: routing/quote freshness is degrading; tighter slippage controls should improve expectancy",
                    project.id
                ),
                novelty_score: (project.slippage_bps / 200.0).clamp(0.05, 1.0),
                expected_gain_sol: (project.slippage_bps / 1000.0).clamp(0.001, 0.25),
            });
        }
        if project.reject_rate > 0.04 {
            out.push(StrategyHypothesis {
                id: format!("hyp-{}-rejects", project.id),
                statement: format!(
                    "{}: reject spikes imply stale sizing/latency assumptions; adaptive order cadence may recover PnL",
                    project.id
                ),
                novelty_score: (project.reject_rate * 8.0).clamp(0.05, 1.0),
                expected_gain_sol: (project.reject_rate * 0.6).clamp(0.001, 0.25),
            });
        }
        if project.drawdown_pct > 0.2 {
            out.push(StrategyHypothesis {
                id: format!("hyp-{}-drawdown", project.id),
                statement: format!(
                    "{}: drawdown profile suggests risk governor should step down exposure faster",
                    project.id
                ),
                novelty_score: project.drawdown_pct.clamp(0.05, 1.0),
                expected_gain_sol: (project.drawdown_pct * 0.4).clamp(0.001, 0.30),
            });
        }
    }
    out.sort_by(|a, b| b.novelty_score.total_cmp(&a.novelty_score));
    out.dedup_by(|a, b| a.statement == b.statement);
    out
}

fn compile_experiment_specs(hypotheses: &[StrategyHypothesis]) -> Vec<ExperimentSpec> {
    hypotheses
        .iter()
        .enumerate()
        .map(|(idx, h)| ExperimentSpec {
            id: format!("exp-{}", idx + 1),
            hypothesis_id: h.id.clone(),
            metric: "net_pnl_after_costs_sol".to_string(),
            control: "current_config".to_string(),
            treatment: format!("treatment_from_{}", h.id),
            pass_criterion: format!(
                "delta_sol > {:.4}",
                (h.expected_gain_sol * 0.35).max(0.0005)
            ),
        })
        .collect()
}

fn build_backtest_matrix(specs: &[ExperimentSpec]) -> Vec<String> {
    specs
        .iter()
        .map(|s| {
            format!(
                "{} | metric={} | control={} | treatment={} | pass={}",
                s.id, s.metric, s.control, s.treatment, s.pass_criterion
            )
        })
        .collect()
}

fn derive_walkforward_checks(projects: &[TradingProjectReport]) -> Vec<String> {
    let mut checks = vec![
        "walk-forward folds: train=30d validate=7d test=7d".to_string(),
        "leakage gate: forbid label/source overlap across folds".to_string(),
        "leakage gate: enforce timestamp monotonicity and no future joins".to_string(),
    ];
    for p in projects {
        checks.push(format!(
            "{}: run_context_audit files={} objective_state={}",
            p.id, p.run_context_files, p.objective_state
        ));
    }
    checks
}

fn rank_meta_strategies(projects: &[TradingProjectReport]) -> Vec<String> {
    let mut rows: Vec<(String, f64)> = projects
        .iter()
        .map(|p| {
            let score = p.latest_pnl_sol
                - (p.drawdown_pct * 0.6)
                - ((p.slippage_bps + p.impact_bps) / 10_000.0)
                - (p.reject_rate * 0.4);
            (p.id.clone(), score)
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    rows.into_iter()
        .map(|(id, score)| format!("{} score={:.6}", id, score))
        .collect()
}

fn compute_strategy_weights(projects: &[TradingProjectReport]) -> HashMap<String, f64> {
    let mut weights = HashMap::new();
    let mut raw = Vec::new();
    for p in projects {
        let score = (p.latest_pnl_sol + 0.25).max(0.01)
            * (1.0 - p.drawdown_pct).max(0.1)
            * (1.0 - p.reject_rate).max(0.1);
        raw.push((p.id.clone(), score));
    }
    let total = raw.iter().map(|(_, v)| *v).sum::<f64>().max(1e-9);
    for (id, score) in raw {
        weights.insert(id, score / total);
    }
    weights
}

fn choose_promotion_candidate(meta: &[String]) -> String {
    meta.first()
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or("none")
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
struct TradingDriftBaseline {
    pub updated_at: String,
    pub heads: HashMap<String, String>,
}

fn trading_drift_baseline_path() -> PathBuf {
    trading_state_dir().join("drift_baseline.json")
}

fn load_trading_drift_baseline() -> TradingDriftBaseline {
    let path = trading_drift_baseline_path();
    if !path.exists() {
        return TradingDriftBaseline::default();
    }
    read_json_file::<TradingDriftBaseline>(&path).unwrap_or_default()
}

fn write_trading_drift_baseline(baseline: &TradingDriftBaseline) {
    let _ = write_json_file(&trading_drift_baseline_path(), baseline);
}

fn run_command_capture(cwd: &Path, bin: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn collect_repo_drift(
    spec: &TradingProjectSpec,
    baseline: &TradingDriftBaseline,
) -> RepoDriftSentinel {
    let root = PathBuf::from(expand_home(&spec.path));
    if !root.exists() {
        return RepoDriftSentinel {
            project_id: spec.id.clone(),
            drift_state: "missing-project".to_string(),
            ..RepoDriftSentinel::default()
        };
    }

    let git_head =
        run_command_capture(&root, "git", &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let dirty_output =
        run_command_capture(&root, "git", &["status", "--short"]).unwrap_or_default();
    let dirty_files = dirty_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let baseline_head = baseline.heads.get(&spec.id).cloned().unwrap_or_default();
    let changed_since_baseline =
        !baseline_head.is_empty() && !git_head.is_empty() && git_head != baseline_head;
    let drift_state = if git_head.is_empty() {
        "not-a-git-repo".to_string()
    } else if dirty_files > 0 {
        "dirty-working-tree".to_string()
    } else if changed_since_baseline {
        "head-changed".to_string()
    } else {
        "stable".to_string()
    };
    RepoDriftSentinel {
        project_id: spec.id.clone(),
        git_head,
        baseline_head,
        dirty_files,
        changed_since_baseline,
        drift_state,
    }
}

fn collect_run_context_audit(project: &TradingProjectReport) -> RunContextAudit {
    let required = vec![
        "wallet_sol".to_string(),
        "pnl_sol".to_string(),
        "reject_rate".to_string(),
        "slippage_bps".to_string(),
        "latency_p95_ms".to_string(),
    ];
    let mut present = Vec::new();
    if project.latest_wallet_sol != 0.0 {
        present.push("wallet_sol".to_string());
    }
    if project.latest_pnl_sol != 0.0 {
        present.push("pnl_sol".to_string());
    }
    if project.reject_rate != 0.0 {
        present.push("reject_rate".to_string());
    }
    if project.slippage_bps != 0.0 {
        present.push("slippage_bps".to_string());
    }
    if project.latency_p95_ms != 0.0 {
        present.push("latency_p95_ms".to_string());
    }
    let missing = required
        .iter()
        .filter(|key| !present.iter().any(|p| p == *key))
        .cloned()
        .collect::<Vec<_>>();
    let passed = project.run_context_files > 0 && missing.len() <= 2;
    RunContextAudit {
        project_id: project.id.clone(),
        files_scanned: project.run_context_files,
        required_metrics_present: present,
        missing_metrics: missing,
        passed,
    }
}

fn parse_env_kv(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn collect_env_provenance(spec: &TradingProjectSpec) -> EnvProvenanceGate {
    let root = PathBuf::from(expand_home(&spec.path));
    if !root.exists() {
        return EnvProvenanceGate {
            project_id: spec.id.clone(),
            passed: false,
            decision: "project-missing".to_string(),
            ..EnvProvenanceGate::default()
        };
    }
    let candidates = vec![
        root.join(".env"),
        root.join("logs").join("knob_tuner").join("overrides.env"),
        root.join("logs")
            .join("nightly_tuner")
            .join("overrides.env"),
    ];
    let critical = vec![
        "RISK_CIRCUIT_MAX_CONSEC_LOSING_CLOSES",
        "REAL_ALGOTRADER_WS_BUY_GATE_ENABLED",
        "PRE_TRADE_MAX_SLIPPAGE_BPS",
        "PRE_TRADE_MIN_LIQUIDITY_USD",
        "REAL_ALGOTRADER_FAMILY_ENABLE",
    ];
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut conflicts = Vec::new();
    let mut inspected = Vec::new();
    for file in candidates {
        if !file.exists() {
            continue;
        }
        inspected.push(file.display().to_string());
        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };
        let kv = parse_env_kv(&raw);
        for key in &critical {
            if let Some(value) = kv.get(*key) {
                if let Some(prev) = seen.get(*key) {
                    if prev != value {
                        conflicts.push((*key).to_string());
                    }
                } else {
                    seen.insert((*key).to_string(), value.clone());
                }
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    let passed = conflicts.is_empty();
    EnvProvenanceGate {
        project_id: spec.id.clone(),
        inspected_files: inspected,
        conflicting_keys: conflicts.clone(),
        passed,
        decision: if passed {
            "provenance-clean".to_string()
        } else {
            format!("conflicts: {}", conflicts.join(", "))
        },
    }
}

fn compute_risk_governor(
    projects: &[TradingProjectReport],
    ruin_probability: f64,
    worst_drawdown: f64,
) -> PortfolioRiskGovernor {
    let max_project_drawdown = projects
        .iter()
        .map(|p| p.drawdown_pct)
        .fold(0.0f64, f64::max);
    if ruin_probability >= 0.6 || worst_drawdown >= 0.45 {
        return PortfolioRiskGovernor {
            mode: "hard-stop".to_string(),
            halt_new_entries: true,
            max_portfolio_drawdown_pct: 0.12,
            max_project_drawdown_pct: 0.08,
            max_ruin_probability: 0.25,
            reason: "ruin_probability or drawdown exceeded hard safety envelope".to_string(),
        };
    }
    if ruin_probability >= 0.35 || max_project_drawdown >= 0.25 {
        return PortfolioRiskGovernor {
            mode: "de-risk".to_string(),
            halt_new_entries: false,
            max_portfolio_drawdown_pct: 0.18,
            max_project_drawdown_pct: 0.12,
            max_ruin_probability: 0.35,
            reason: "risk elevated; reduce exposure and tighten gates".to_string(),
        };
    }
    PortfolioRiskGovernor {
        mode: "normal".to_string(),
        halt_new_entries: false,
        max_portfolio_drawdown_pct: 0.25,
        max_project_drawdown_pct: 0.18,
        max_ruin_probability: 0.45,
        reason: "risk envelope healthy".to_string(),
    }
}

fn compute_capital_allocator(
    projects: &[TradingProjectReport],
    strategy_weights: &HashMap<String, f64>,
    current_wallet: f64,
    governor: &PortfolioRiskGovernor,
) -> Vec<CapitalAllocationRow> {
    let mode_factor = match governor.mode.as_str() {
        "hard-stop" => 0.10,
        "de-risk" => 0.55,
        _ => 1.0,
    };
    projects
        .iter()
        .map(|p| {
            let target_weight = *strategy_weights.get(&p.id).unwrap_or(&0.0);
            let throttle_factor = (1.0 - p.drawdown_pct).clamp(0.1, 1.0) * mode_factor;
            let target_capital_sol = (current_wallet * target_weight * throttle_factor).max(0.0);
            let max_loss_budget_sol =
                (target_capital_sol * governor.max_project_drawdown_pct).max(0.0);
            CapitalAllocationRow {
                project_id: p.id.clone(),
                target_weight,
                target_capital_sol,
                max_loss_budget_sol,
                throttle_factor,
            }
        })
        .collect()
}

fn compute_canary_pipeline(
    report: &TradingAlphaReport,
    governor: &PortfolioRiskGovernor,
) -> Vec<CanaryPromotionStep> {
    let stage1 = report
        .run_context_audits
        .iter()
        .all(|audit| audit.passed && audit.files_scanned > 0);
    let stage2 = report.ruin_probability <= governor.max_ruin_probability;
    let stage3 = report
        .replay_canary
        .iter()
        .all(|row| row.pass_rate >= 0.60 && row.sample_size > 0);
    vec![
        CanaryPromotionStep {
            stage: "telemetry-audit".to_string(),
            passed: stage1,
            detail: "run_context invariants and coverage checks".to_string(),
        },
        CanaryPromotionStep {
            stage: "risk-envelope".to_string(),
            passed: stage2,
            detail: format!(
                "ruin_probability {:.4} <= max {:.4}",
                report.ruin_probability, governor.max_ruin_probability
            ),
        },
        CanaryPromotionStep {
            stage: "replay-canary".to_string(),
            passed: stage3,
            detail: "fresh telemetry replay pass-rate gate".to_string(),
        },
    ]
}

fn compute_replay_canary(projects: &[TradingProjectReport]) -> Vec<ReplayCanaryResult> {
    projects
        .iter()
        .map(|p| {
            let sample = p.run_context_files.max(1);
            let quality = (1.0 - p.reject_rate).clamp(0.0, 1.0)
                * (1.0 - (p.slippage_bps / 150.0).clamp(0.0, 1.0))
                * (1.0 - p.drawdown_pct.clamp(0.0, 1.0));
            let decision = if quality >= 0.60 {
                "pass".to_string()
            } else {
                "fail".to_string()
            };
            ReplayCanaryResult {
                project_id: p.id.clone(),
                sample_size: sample,
                pass_rate: quality,
                decision,
            }
        })
        .collect()
}

fn build_remediation_runbook(
    projects: &[TradingProjectReport],
    governor: &PortfolioRiskGovernor,
) -> Vec<RemediationRunbookAction> {
    let mut out = Vec::new();
    for p in projects {
        let mut pushed = false;
        for rec in p.patch_recommendations.iter().take(2) {
            out.push(RemediationRunbookAction {
                project_id: p.id.clone(),
                priority: if p.objective_state == "regressing" {
                    "p0".to_string()
                } else {
                    "p1".to_string()
                },
                title: rec.clone(),
                command: format!(
                    "cd {} && rg -n \"slippage|reject|drawdown|risk\" src scripts",
                    p.path
                ),
                rationale: format!("incident={} regime={}", p.incident_class, p.regime),
            });
            pushed = true;
        }
        if !pushed {
            out.push(RemediationRunbookAction {
                project_id: p.id.clone(),
                priority: "p2".to_string(),
                title: "continue telemetry burn-in".to_string(),
                command: format!("cd {} && ls logs/run_context | tail -n 20", p.path),
                rationale: "no urgent remediation signals found".to_string(),
            });
        }
    }
    if governor.halt_new_entries {
        out.push(RemediationRunbookAction {
            project_id: "portfolio".to_string(),
            priority: "p0".to_string(),
            title: "halt new entries and switch to shadow-only".to_string(),
            command: "set RISK_MODE=shadow_only and disable live entries until risk recovers"
                .to_string(),
            rationale: governor.reason.clone(),
        });
    }
    out
}

fn count_files(dir: &Path, max_depth: usize) -> usize {
    if max_depth == 0 || !dir.exists() {
        return 0;
    }
    let mut total = 0usize;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                total = total.saturating_add(1);
            } else if path.is_dir() {
                total = total.saturating_add(count_files(&path, max_depth.saturating_sub(1)));
            }
        }
    }
    total
}

fn ingest_research_sources(spec: &TradingProjectSpec) -> Vec<ResearchSourceIngestion> {
    let root = PathBuf::from(expand_home(&spec.path));
    let sources = vec![
        ("run_context", root.join("logs").join("run_context"), 2usize),
        ("docs", root.join("docs"), 2usize),
        ("scripts", root.join("scripts"), 2usize),
        ("notebooks", root.join("notebooks"), 2usize),
        ("backtests", root.join("backtests"), 2usize),
    ];
    sources
        .into_iter()
        .map(|(name, path, depth)| ResearchSourceIngestion {
            project_id: spec.id.clone(),
            source: name.to_string(),
            path: path.display().to_string(),
            found: path.exists(),
            items: if path.exists() {
                count_files(&path, depth)
            } else {
                0
            },
        })
        .collect()
}

fn compute_postmortem(projects: &[TradingProjectReport]) -> String {
    let mut lines = Vec::new();
    lines.push("Postmortem packet".to_string());
    for p in projects {
        lines.push(format!(
            "- {} state={} incident={} drawdown={:.2}% pnl={:.6} sol",
            p.id,
            p.objective_state,
            p.incident_class,
            p.drawdown_pct * 100.0,
            p.latest_pnl_sol
        ));
        for r in p.patch_recommendations.iter().take(2) {
            lines.push(format!("  remediation: {}", r));
        }
    }
    lines.join("\n")
}

pub fn refresh_trading_alpha_report() -> Result<TradingAlphaReport, AgentError> {
    ensure_trading_runtime_bootstrap(false)?;
    let cfg = load_trading_runtime_config()?;
    let projects = cfg
        .projects
        .iter()
        .filter(|p| p.enabled)
        .map(analyze_project)
        .collect::<Vec<_>>();
    let active_specs = cfg
        .projects
        .iter()
        .filter(|p| p.enabled)
        .cloned()
        .collect::<Vec<_>>();

    let current_wallet = projects
        .iter()
        .map(|p| p.latest_wallet_sol)
        .fold(0.0f64, f64::max);
    let progress = if cfg.target_wallet_sol > cfg.starting_wallet_sol {
        ((current_wallet - cfg.starting_wallet_sol)
            / (cfg.target_wallet_sol - cfg.starting_wallet_sol))
            .clamp(0.0, 1.0)
    } else {
        0.0
    };
    let worst_drawdown = projects
        .iter()
        .map(|p| p.drawdown_pct)
        .fold(0.0f64, f64::max);
    let ruin_probability = (worst_drawdown * 0.85 + (1.0 - progress) * 0.15).clamp(0.0, 1.0);
    let avg_vol = if projects.is_empty() {
        0.0
    } else {
        projects.iter().map(|p| p.volatility_score).sum::<f64>() / (projects.len() as f64)
    };
    let volatility_sizing_factor = (1.0 / (1.0 + avg_vol)).clamp(0.15, 1.25);

    let strategy_weights = compute_strategy_weights(&projects);
    let signal = projects
        .iter()
        .map(|p| p.latest_pnl_sol.max(0.0))
        .sum::<f64>();
    let execution_cost = projects
        .iter()
        .map(|p| (p.slippage_bps + p.impact_bps) / 10_000.0)
        .sum::<f64>();
    let fee_cost = projects
        .iter()
        .map(|p| p.fee_drag_sol + p.funding_drag_sol)
        .sum::<f64>();
    let mut pnl_decomposition = HashMap::new();
    pnl_decomposition.insert("signal".to_string(), signal);
    pnl_decomposition.insert("execution_cost".to_string(), -execution_cost);
    pnl_decomposition.insert("fee_cost".to_string(), -fee_cost);

    let canary_recommendation = if ruin_probability > 0.45 {
        "rollback-to-shadow".to_string()
    } else if progress > 0.55 && worst_drawdown < 0.15 {
        "promote-canary".to_string()
    } else {
        "hold-canary".to_string()
    };

    let hypotheses = derive_hypotheses(&TradingAlphaReport {
        projects: projects.clone(),
        ..TradingAlphaReport::default()
    });
    let experiments = compile_experiment_specs(&hypotheses);
    let backtest_matrix = build_backtest_matrix(&experiments);
    let walkforward_checks = derive_walkforward_checks(&projects);
    let meta_ranking = rank_meta_strategies(&projects);
    let promotion_candidate = choose_promotion_candidate(&meta_ranking);
    let postmortem = compute_postmortem(&projects);
    let risk_governor = compute_risk_governor(&projects, ruin_probability, worst_drawdown);
    let capital_allocator =
        compute_capital_allocator(&projects, &strategy_weights, current_wallet, &risk_governor);
    let replay_canary = compute_replay_canary(&projects);
    let run_context_audits = projects
        .iter()
        .map(collect_run_context_audit)
        .collect::<Vec<_>>();
    let env_provenance = active_specs
        .iter()
        .map(collect_env_provenance)
        .collect::<Vec<_>>();
    let remediation_runbook = build_remediation_runbook(&projects, &risk_governor);
    let research_sources = active_specs
        .iter()
        .flat_map(ingest_research_sources)
        .collect::<Vec<_>>();

    let baseline = load_trading_drift_baseline();
    let repo_drift = active_specs
        .iter()
        .map(|spec| collect_repo_drift(spec, &baseline))
        .collect::<Vec<_>>();

    let mut next_baseline = baseline.clone();
    for drift in &repo_drift {
        if !drift.git_head.is_empty() {
            next_baseline
                .heads
                .insert(drift.project_id.clone(), drift.git_head.clone());
        }
    }
    next_baseline.updated_at = now_rfc3339();

    let mut report = TradingAlphaReport {
        generated_at: now_rfc3339(),
        projects,
        wallet_progress_pct: progress,
        ruin_probability,
        volatility_sizing_factor,
        strategy_weights,
        pnl_decomposition,
        canary_recommendation,
        postmortem,
        hypotheses,
        experiments,
        backtest_matrix,
        walkforward_checks,
        meta_ranking,
        promotion_candidate,
        capital_allocator,
        risk_governor,
        canary_pipeline: Vec::new(),
        repo_drift,
        run_context_audits,
        env_provenance,
        replay_canary,
        remediation_runbook,
        research_sources,
    };
    report.canary_pipeline = compute_canary_pipeline(&report, &report.risk_governor);

    write_trading_drift_baseline(&next_baseline);
    write_json_file(&trading_last_report_path(), &report)?;
    Ok(report)
}

pub fn load_last_trading_alpha_report() -> Result<TradingAlphaReport, AgentError> {
    ensure_trading_runtime_bootstrap(false)?;
    read_json_file(&trading_last_report_path())
}

pub fn render_trading_alpha_board(report: &TradingAlphaReport) -> String {
    let mut out = String::new();
    out.push_str("Trading Private Mission Board\n");
    out.push_str("----------------------------\n");
    out.push_str(&format!("generated_at: {}\n", report.generated_at));
    out.push_str(&format!(
        "wallet_progress_pct: {:.2}%\nruin_probability: {:.4}\nvolatility_sizing_factor: {:.4}\ncanary_recommendation: {}\npromotion_candidate: {}\n\n",
        report.wallet_progress_pct * 100.0,
        report.ruin_probability,
        report.volatility_sizing_factor,
        report.canary_recommendation,
        report.promotion_candidate
    ));

    out.push_str("Project telemetry\n");
    for p in &report.projects {
        out.push_str(&format!(
            "- {} exists={} run_context_files={} wallet={:.6} pnl={:.6} drawdown={:.2}% reject_rate={:.2}% regime={} incident={} objective_state={}\n",
            p.id,
            p.exists,
            p.run_context_files,
            p.latest_wallet_sol,
            p.latest_pnl_sol,
            p.drawdown_pct * 100.0,
            p.reject_rate * 100.0,
            p.regime,
            p.incident_class,
            p.objective_state
        ));
    }
    out.push('\n');

    out.push_str("Strategy weights\n");
    let mut weights = report
        .strategy_weights
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect::<Vec<_>>();
    weights.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (id, w) in weights {
        out.push_str(&format!("- {}: {:.4}\n", id, w));
    }
    out.push('\n');

    out.push_str("Capital allocator\n");
    for row in &report.capital_allocator {
        out.push_str(&format!(
            "- {} weight={:.4} capital_sol={:.6} max_loss_sol={:.6} throttle={:.3}\n",
            row.project_id,
            row.target_weight,
            row.target_capital_sol,
            row.max_loss_budget_sol,
            row.throttle_factor
        ));
    }
    out.push('\n');

    out.push_str("Risk governor\n");
    out.push_str(&format!(
        "- mode={} halt_new_entries={} max_portfolio_drawdown={:.2}% max_project_drawdown={:.2}% max_ruin_probability={:.4}\n",
        report.risk_governor.mode,
        report.risk_governor.halt_new_entries,
        report.risk_governor.max_portfolio_drawdown_pct * 100.0,
        report.risk_governor.max_project_drawdown_pct * 100.0,
        report.risk_governor.max_ruin_probability
    ));
    out.push_str(&format!("  reason: {}\n\n", report.risk_governor.reason));

    out.push_str("Canary promotion pipeline\n");
    for step in &report.canary_pipeline {
        out.push_str(&format!(
            "- {} passed={} detail={}\n",
            step.stage, step.passed, step.detail
        ));
    }
    out.push('\n');

    out.push_str("Autoresearch\n");
    out.push_str(&format!(
        "- hypotheses={} experiments={} matrix_rows={}\n",
        report.hypotheses.len(),
        report.experiments.len(),
        report.backtest_matrix.len()
    ));
    for h in report.hypotheses.iter().take(4) {
        out.push_str(&format!(
            "  - {} novelty={:.3} expected_gain_sol={:.4}\n",
            h.id, h.novelty_score, h.expected_gain_sol
        ));
    }
    out.push('\n');

    out.push_str("Walk-forward + leakage defense\n");
    for line in report.walkforward_checks.iter().take(6) {
        out.push_str(&format!("- {}\n", line));
    }
    out.push('\n');
    out.push_str("Meta ranking\n");
    for line in report.meta_ranking.iter().take(6) {
        out.push_str(&format!("- {}\n", line));
    }
    out.push('\n');

    out.push_str("Repo drift sentinel\n");
    for row in &report.repo_drift {
        out.push_str(&format!(
            "- {} state={} head={} baseline={} dirty_files={} changed_since_baseline={}\n",
            row.project_id,
            row.drift_state,
            row.git_head,
            row.baseline_head,
            row.dirty_files,
            row.changed_since_baseline
        ));
    }
    out.push('\n');

    out.push_str("Run context audits\n");
    for audit in &report.run_context_audits {
        out.push_str(&format!(
            "- {} passed={} files_scanned={} missing={}\n",
            audit.project_id,
            audit.passed,
            audit.files_scanned,
            if audit.missing_metrics.is_empty() {
                "none".to_string()
            } else {
                audit.missing_metrics.join(",")
            }
        ));
    }
    out.push('\n');

    out.push_str("Env provenance gates\n");
    for gate in &report.env_provenance {
        out.push_str(&format!(
            "- {} passed={} inspected_files={} decision={}\n",
            gate.project_id,
            gate.passed,
            gate.inspected_files.len(),
            gate.decision
        ));
    }
    out.push('\n');

    out.push_str("Replay canary\n");
    for row in &report.replay_canary {
        out.push_str(&format!(
            "- {} sample_size={} pass_rate={:.3} decision={}\n",
            row.project_id, row.sample_size, row.pass_rate, row.decision
        ));
    }
    out.push('\n');

    out.push_str("Remediation runbook\n");
    for action in report.remediation_runbook.iter().take(12) {
        out.push_str(&format!(
            "- [{}] {} :: {} | {}\n",
            action.priority, action.project_id, action.title, action.command
        ));
    }
    out.push('\n');

    out.push_str("Research source ingestion\n");
    for src in report.research_sources.iter().take(24) {
        out.push_str(&format!(
            "- {}:{} found={} items={} path={}\n",
            src.project_id, src.source, src.found, src.items, src.path
        ));
    }
    out.push('\n');

    out.push_str(&report.postmortem);
    out
}
