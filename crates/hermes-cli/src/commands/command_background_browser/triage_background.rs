#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum TriggerTriageDecision {
    Drop,
    Notify,
    Escalate,
    AgentRun,
}

impl TriggerTriageDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::Notify => "notify",
            Self::Escalate => "escalate",
            Self::AgentRun => "agent-run",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriggerTriageAssessment {
    source: String,
    payload: String,
    severity: i32,
    decision: TriggerTriageDecision,
    requires_approval: bool,
    reasons: Vec<String>,
}

fn trigger_triage_mode() -> String {
    std::env::var("HERMES_TRIGGER_TRIAGE_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "off".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriggerTriageLearningEntry {
    at: String,
    source: String,
    outcome: String,
    decision: String,
    severity: i32,
    bias_delta: i32,
    note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TriggerTriageLearningState {
    #[serde(default)]
    entries: Vec<TriggerTriageLearningEntry>,
}

fn trigger_triage_learning_state_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("triage")
        .join("learning.json")
}

fn load_trigger_triage_learning_state() -> TriggerTriageLearningState {
    let path = trigger_triage_learning_state_path();
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str::<TriggerTriageLearningState>(&raw).unwrap_or_default()
}

fn save_trigger_triage_learning_state(
    state: &TriggerTriageLearningState,
) -> Result<(), AgentError> {
    let path = trigger_triage_learning_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let payload = serde_json::to_string_pretty(state)
        .map_err(|e| AgentError::Io(format!("Failed to encode triage learning state: {}", e)))?;
    std::fs::write(&path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn triage_feedback_delta(outcome: &str) -> Option<i32> {
    match outcome.trim().to_ascii_lowercase().as_str() {
        "critical" | "escalate" | "confirmed" | "true-positive" | "tp" => Some(2),
        "useful" | "good" | "notify" | "watch" => Some(1),
        "neutral" | "mixed" => Some(0),
        "false-positive" | "fp" | "noise" | "noisy" => Some(-2),
        "drop" | "ignore" | "spam" => Some(-1),
        _ => None,
    }
}

fn triage_learning_bias(source: &str, payload: &str) -> (i32, Vec<String>) {
    let source_l = source.trim().to_ascii_lowercase();
    let payload_l = payload.trim().to_ascii_lowercase();
    let state = load_trigger_triage_learning_state();
    let mut total = 0i32;
    let mut reasons = Vec::new();
    for entry in state.entries.iter().rev().take(120) {
        if entry.source.eq_ignore_ascii_case(&source_l) {
            total += entry.bias_delta;
            if reasons.len() < 3 {
                reasons.push(format!(
                    "source feedback {} ({})",
                    entry.outcome, entry.bias_delta
                ));
            }
            continue;
        }
        if !entry.note.trim().is_empty()
            && payload_l.contains(entry.note.trim().to_ascii_lowercase().as_str())
        {
            total += entry.bias_delta.signum();
            if reasons.len() < 3 {
                reasons.push(format!("matched prior note '{}'", entry.note));
            }
        }
    }
    (total.clamp(-3, 3), reasons)
}

fn evaluate_trigger_triage(source: &str, payload: &str) -> TriggerTriageAssessment {
    let source_l = source.trim().to_ascii_lowercase();
    let payload_l = payload.trim().to_ascii_lowercase();
    let mode = trigger_triage_mode();
    let mut severity = 0i32;
    let mut reasons = Vec::new();

    for (needle, score, reason) in [
        ("panic", 4, "runtime panic or crash"),
        ("outage", 4, "service outage signal"),
        ("secret", 5, "secret exposure indicator"),
        ("key leak", 5, "key leak indicator"),
        ("drawdown", 4, "drawdown or loss event"),
        ("halt", 3, "trading halt or critical gate"),
        ("blocked", 2, "policy or sandbox block"),
        ("timeout", 1, "timeout/retry pressure"),
        ("latency", 1, "latency degradation"),
        ("error", 2, "error signal"),
    ] {
        if payload_l.contains(needle) || source_l.contains(needle) {
            severity += score;
            reasons.push(reason.to_string());
        }
    }

    if source_l.contains("webhook") {
        severity += 1;
        reasons.push("external webhook trigger".to_string());
    }
    if source_l.contains("cron") {
        severity += 1;
        reasons.push("scheduled trigger".to_string());
    }

    let (learning_bias, learning_reasons) = triage_learning_bias(source, payload);
    if learning_bias != 0 {
        severity += learning_bias;
        reasons.push(format!("learning bias applied ({:+})", learning_bias));
        reasons.extend(learning_reasons);
    }

    if mode == "strict" {
        severity += 1;
    } else if mode == "relaxed" {
        severity = severity.saturating_sub(1);
    }

    let (decision, requires_approval) = if severity >= 7 {
        (TriggerTriageDecision::Escalate, true)
    } else if severity >= 4 {
        (TriggerTriageDecision::AgentRun, false)
    } else if severity >= 2 {
        (TriggerTriageDecision::Notify, false)
    } else if payload_l.len() < 6 {
        (TriggerTriageDecision::Drop, false)
    } else {
        (TriggerTriageDecision::Notify, false)
    };

    TriggerTriageAssessment {
        source: source.trim().to_string(),
        payload: payload.trim().to_string(),
        severity,
        decision,
        requires_approval,
        reasons,
    }
}

fn render_trigger_triage_assessment(assessment: &TriggerTriageAssessment) -> String {
    let mut out = String::new();
    out.push_str("Trigger triage assessment\n");
    out.push_str("------------------------\n");
    let _ = writeln!(out, "source: {}", assessment.source);
    let _ = writeln!(out, "payload: {}", truncate_chars(&assessment.payload, 220));
    let _ = writeln!(out, "severity: {}", assessment.severity);
    let _ = writeln!(out, "decision: {}", assessment.decision.as_str());
    let _ = writeln!(out, "requires_approval: {}", assessment.requires_approval);
    if assessment.reasons.is_empty() {
        out.push_str("reasons: none\n");
    } else {
        out.push_str("reasons:\n");
        for reason in &assessment.reasons {
            let _ = writeln!(out, "- {}", reason);
        }
    }
    out
}

fn append_triage_learning_feedback(
    source: &str,
    payload: &str,
    outcome: &str,
    assessment: &TriggerTriageAssessment,
) -> Result<TriggerTriageLearningEntry, AgentError> {
    let delta = triage_feedback_delta(outcome).ok_or_else(|| {
        AgentError::Config(
            "Unknown triage feedback outcome. Use critical|confirmed|useful|neutral|false-positive|drop."
                .to_string(),
        )
    })?;
    let note = payload
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    let entry = TriggerTriageLearningEntry {
        at: chrono::Utc::now().to_rfc3339(),
        source: source.trim().to_ascii_lowercase(),
        outcome: outcome.trim().to_ascii_lowercase(),
        decision: assessment.decision.as_str().to_string(),
        severity: assessment.severity,
        bias_delta: delta,
        note,
    };
    let mut state = load_trigger_triage_learning_state();
    state.entries.push(entry.clone());
    if state.entries.len() > 400 {
        let remove = state.entries.len().saturating_sub(400);
        state.entries.drain(0..remove);
    }
    save_trigger_triage_learning_state(&state)?;
    Ok(entry)
}

fn render_trigger_triage_learning_status() -> String {
    let state = load_trigger_triage_learning_state();
    let mut by_source: HashMap<String, i32> = HashMap::new();
    for entry in &state.entries {
        *by_source.entry(entry.source.clone()).or_insert(0) += entry.bias_delta;
    }
    let mut ranked = by_source.into_iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(_, bias)| std::cmp::Reverse(*bias));
    let mut out = String::new();
    out.push_str("Trigger triage learning\n");
    out.push_str("----------------------\n");
    let _ = writeln!(out, "entries: {}", state.entries.len());
    if ranked.is_empty() {
        out.push_str("source_bias: none\n");
    } else {
        out.push_str("source_bias:\n");
        for (source, bias) in ranked.into_iter().take(6) {
            let _ = writeln!(out, "- {} => {:+}", source, bias);
        }
    }
    if let Some(last) = state.entries.last() {
        let _ = writeln!(
            out,
            "last_feedback: {} source={} outcome={} delta={:+}",
            last.at, last.source, last.outcome, last.bias_delta
        );
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubconsciousTask {
    id: String,
    source: String,
    prompt: String,
    score: f64,
    risk: String,
    requires_approval: bool,
    status: String,
    #[serde(default)]
    job_id: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SubconsciousQueueState {
    #[serde(default)]
    tasks: Vec<SubconsciousTask>,
}

fn subconscious_state_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("subconscious")
        .join("queue.json")
}

fn load_subconscious_state() -> SubconsciousQueueState {
    let path = subconscious_state_path();
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str::<SubconsciousQueueState>(&raw).unwrap_or_default()
}

fn save_subconscious_state(state: &SubconsciousQueueState) -> Result<(), AgentError> {
    let path = subconscious_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let payload = serde_json::to_string_pretty(state)
        .map_err(|e| AgentError::Io(format!("Failed to encode subconscious state: {}", e)))?;
    std::fs::write(&path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn score_subconscious_task(prompt: &str) -> f64 {
    let text = prompt.to_ascii_lowercase();
    let mut score = 1.0f64;
    if text.contains("profit")
        || text.contains("wallet")
        || text.contains("sol")
        || text.contains("latency")
        || text.contains("regression")
    {
        score += 1.2;
    }
    if text.contains("fix") || text.contains("verify") || text.contains("test") {
        score += 0.8;
    }
    if let Ok(terms) = utility_terms_from_contract() {
        let mut overlap = 0.0f64;
        for (term, weight) in terms {
            if text.contains(&term.to_ascii_lowercase()) {
                overlap += weight.max(0.0);
            }
        }
        score += overlap.min(2.5);
    }
    score
}

fn risk_for_prompt(prompt: &str) -> (&'static str, bool) {
    let text = prompt.to_ascii_lowercase();
    if text.contains("rm -rf")
        || text.contains("delete ")
        || text.contains("rotate key")
        || text.contains("prod")
        || text.contains("mainnet")
    {
        return ("high", true);
    }
    if text.contains("live trading") || text.contains("wallet") || text.contains("deploy") {
        return ("medium", true);
    }
    ("low", false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubconsciousProfile {
    Strict,
    Balanced,
    Dev,
}

impl SubconsciousProfile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Balanced => "balanced",
            Self::Dev => "dev",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(Self::Strict),
            "balanced" | "standard" => Some(Self::Balanced),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }
}

fn subconscious_profile_env() -> SubconsciousProfile {
    std::env::var("HERMES_SUBCONSCIOUS_PROFILE")
        .ok()
        .and_then(|v| SubconsciousProfile::parse(&v))
        .unwrap_or(SubconsciousProfile::Balanced)
}

fn subconscious_guard_allows(
    profile: SubconsciousProfile,
    task: &SubconsciousTask,
) -> (bool, String) {
    let risk = task.risk.to_ascii_lowercase();
    match profile {
        SubconsciousProfile::Dev => (true, "dev profile allows execution".to_string()),
        SubconsciousProfile::Balanced => {
            if risk == "high" {
                (
                    false,
                    "balanced profile blocks high-risk subconscious runs".to_string(),
                )
            } else {
                (true, "balanced profile allows low/medium risk".to_string())
            }
        }
        SubconsciousProfile::Strict => {
            if task.requires_approval || risk != "low" {
                (
                    false,
                    "strict profile allows only low-risk non-approval tasks".to_string(),
                )
            } else {
                (true, "strict profile allows low-risk task".to_string())
            }
        }
    }
}

fn handle_subconscious_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "list" => {
            let state = load_subconscious_state();
            let profile = subconscious_profile_env();
            let mut out = String::new();
            out.push_str("Subconscious queue\n");
            out.push_str("-----------------\n");
            let _ = writeln!(out, "profile: {}", profile.as_str());
            if state.tasks.is_empty() {
                out.push_str("No queued subconscious tasks.\n");
            } else {
                for task in state.tasks.iter().rev().take(24) {
                    let _ = writeln!(
                        out,
                        "- {} [{}] score={:.2} risk={} approval={} source={} :: {}",
                        task.id,
                        task.status,
                        task.score,
                        task.risk,
                        task.requires_approval,
                        task.source,
                        truncate_chars(&task.prompt, 100)
                    );
                }
            }
            out.push_str(
                "\nUsage: /subconscious add <prompt> | approve <id> | reject <id> | run [n] [--dry-run] [profile=<strict|balanced|dev>] | profile [status|list|strict|balanced|dev|clear] | clear",
            );
            emit_command_output(app, out.trim_end());
        }
        "add" => {
            let prompt = args.get(1..).unwrap_or(&[]).join(" ").trim().to_string();
            if prompt.is_empty() {
                emit_command_output(app, "Usage: /subconscious add <prompt>");
                return Ok(CommandResult::Handled);
            }
            let (risk, requires_approval) = risk_for_prompt(&prompt);
            let score = score_subconscious_task(&prompt);
            let mut state = load_subconscious_state();
            let id = format!(
                "sc-{}",
                uuid::Uuid::new_v4()
                    .simple()
                    .to_string()
                    .chars()
                    .take(8)
                    .collect::<String>()
            );
            let task = SubconsciousTask {
                id: id.clone(),
                source: "manual".to_string(),
                prompt,
                score,
                risk: risk.to_string(),
                requires_approval,
                status: if requires_approval {
                    "pending-approval".to_string()
                } else {
                    "pending".to_string()
                },
                job_id: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            };
            state.tasks.push(task.clone());
            save_subconscious_state(&state)?;
            emit_command_output(
                app,
                format!(
                    "Queued subconscious task {}\nstatus={} score={:.2} risk={}\n{}",
                    task.id,
                    task.status,
                    task.score,
                    task.risk,
                    if task.requires_approval {
                        "Requires approval: /subconscious approve <id>"
                    } else {
                        "Ready to run: /subconscious run"
                    }
                ),
            );
        }
        "approve" | "reject" => {
            let Some(task_id) = args.get(1).copied() else {
                emit_command_output(app, format!("Usage: /subconscious {} <id>", action));
                return Ok(CommandResult::Handled);
            };
            let mut state = load_subconscious_state();
            let mut found = false;
            for task in &mut state.tasks {
                if task.id.eq_ignore_ascii_case(task_id) {
                    found = true;
                    task.status = if action == "approve" {
                        "pending".to_string()
                    } else {
                        "rejected".to_string()
                    };
                    task.updated_at = chrono::Utc::now().to_rfc3339();
                    break;
                }
            }
            if !found {
                emit_command_output(app, format!("Task not found: {}", task_id));
                return Ok(CommandResult::Handled);
            }
            save_subconscious_state(&state)?;
            emit_command_output(app, format!("Subconscious task {} {}", task_id, action));
        }
        "run" => {
            let mut limit = 1usize;
            let mut dry_run = false;
            let mut profile_override: Option<SubconsciousProfile> = None;
            for token in args.get(1..).unwrap_or(&[]) {
                let token_l = token.trim().to_ascii_lowercase();
                if token_l == "--dry-run" || token_l == "dry-run" || token_l == "preview" {
                    dry_run = true;
                    continue;
                }
                if let Ok(parsed) = token_l.parse::<usize>() {
                    limit = parsed.clamp(1, 8);
                    continue;
                }
                if let Some(raw) = token_l.strip_prefix("profile=") {
                    profile_override = SubconsciousProfile::parse(raw);
                    continue;
                }
                if profile_override.is_none() {
                    profile_override = SubconsciousProfile::parse(&token_l);
                }
            }
            let profile = profile_override.unwrap_or_else(subconscious_profile_env);
            let mut state = load_subconscious_state();
            let mut reviewed = 0usize;
            let mut dispatched = 0usize;
            let mut blocked = 0usize;
            let mut notes = Vec::new();
            for task in &mut state.tasks {
                if reviewed >= limit {
                    break;
                }
                if task.status != "pending" {
                    continue;
                }
                reviewed += 1;
                let (allowed, guard_note) = subconscious_guard_allows(profile, task);
                if !allowed {
                    blocked += 1;
                    notes.push(format!("{} blocked ({})", task.id, guard_note));
                    continue;
                }
                if dry_run {
                    notes.push(format!("{} would dispatch ({})", task.id, guard_note));
                    continue;
                }
                let job = queue_background_job(&app.state_root, &task.prompt)?;
                task.status = "dispatched".to_string();
                task.job_id = Some(job.id.clone());
                task.updated_at = chrono::Utc::now().to_rfc3339();
                dispatched += 1;
                notes.push(format!("{} dispatched id={}", task.id, job.id));
            }
            if !dry_run {
                save_subconscious_state(&state)?;
            }
            emit_command_output(
                app,
                format!(
                    "{} subconscious run profile={}\nreviewed={} dispatched={} blocked={}\n{}\nUse `/background status` and `/subconscious status` for tracking.",
                    if dry_run {
                        "Dry-run"
                    } else {
                        "Executed"
                    },
                    profile.as_str(),
                    reviewed,
                    dispatched,
                    blocked,
                    if notes.is_empty() {
                        "No pending tasks matched selection.".to_string()
                    } else {
                        notes.join("\n")
                    }
                ),
            );
        }
        "profile" => {
            let token = args.get(1).copied().unwrap_or("status").to_ascii_lowercase();
            match token.as_str() {
                "status" | "show" => emit_command_output(
                    app,
                    format!(
                        "Subconscious profile: {}\nUse `/subconscious profile list` or `/subconscious profile strict|balanced|dev`.",
                        subconscious_profile_env().as_str()
                    ),
                ),
                "list" => emit_command_output(
                    app,
                    "Subconscious profiles:\n- strict: only low-risk non-approval tasks auto-dispatch\n- balanced: low/medium dispatch, high-risk blocked\n- dev: permit all pending tasks\nSet with `/subconscious profile <name>`.",
                ),
                "clear" => {
                    std::env::remove_var("HERMES_SUBCONSCIOUS_PROFILE");
                    emit_command_output(
                        app,
                        "Cleared subconscious profile override (default=balanced).",
                    );
                }
                other => {
                    let Some(next) = SubconsciousProfile::parse(other) else {
                        emit_command_output(
                            app,
                            "Usage: /subconscious profile [status|list|strict|balanced|dev|clear]",
                        );
                        return Ok(CommandResult::Handled);
                    };
                    std::env::set_var("HERMES_SUBCONSCIOUS_PROFILE", next.as_str());
                    emit_command_output(app, format!("Subconscious profile set to {}.", next.as_str()));
                }
            }
        }
        "clear" => {
            let path = subconscious_state_path();
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| {
                    AgentError::Io(format!("Failed to remove {}: {}", path.display(), e))
                })?;
            }
            emit_command_output(app, "Cleared subconscious queue.");
        }
        _ => emit_command_output(
            app,
            "Usage: /subconscious [status|add <prompt>|approve <id>|reject <id>|run [n] [--dry-run] [profile=<strict|balanced|dev>]|profile [status|list|strict|balanced|dev|clear]|clear]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_trigger_triage_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" => {
            emit_command_output(
                app,
                format!(
                    "Trigger triage mode: {}\n{}\nUsage: /triage eval <source> <payload> | /triage queue <source> <payload> | /triage feedback <source> <outcome> <payload>",
                    trigger_triage_mode(),
                    render_trigger_triage_learning_status().trim_end()
                ),
            );
        }
        "list" | "rules" => {
            emit_command_output(
                app,
                "Trigger triage heuristics\n\
                 - high severity: panic/outage/secret leak/drawdown/halt -> escalate\n\
                 - medium severity: repeated errors/blocked/timeout -> agent-run\n\
                 - low severity: notify\n\
                 - empty/noise payload -> drop\n\
                 Mode override: HERMES_TRIGGER_TRIAGE_MODE={strict|balanced|relaxed}\n\
                 Feedback loop: `/triage feedback <source> <outcome> <payload>` updates persistent bias.",
            );
        }
        "feedback" => {
            let Some(source) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    "Usage: /triage feedback <source> <outcome> <payload>",
                );
                return Ok(CommandResult::Handled);
            };
            let Some(outcome) = args.get(2).copied() else {
                emit_command_output(
                    app,
                    "Usage: /triage feedback <source> <outcome> <payload>",
                );
                return Ok(CommandResult::Handled);
            };
            let payload = args.get(3..).unwrap_or(&[]).join(" ").trim().to_string();
            if payload.is_empty() {
                emit_command_output(
                    app,
                    "Usage: /triage feedback <source> <outcome> <payload>",
                );
                return Ok(CommandResult::Handled);
            }
            let assessment = evaluate_trigger_triage(source, &payload);
            let entry = append_triage_learning_feedback(source, &payload, outcome, &assessment)?;
            let (bias_now, _) = triage_learning_bias(source, &payload);
            emit_command_output(
                app,
                format!(
                    "Recorded triage feedback.\nsource={} outcome={} delta={:+} decision={} severity={}\nsource_bias_now={:+}",
                    entry.source, entry.outcome, entry.bias_delta, entry.decision, entry.severity, bias_now
                ),
            );
        }
        "eval" | "queue" => {
            let Some(source) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    "Usage: /triage eval <source> <payload>\nUsage: /triage queue <source> <payload>",
                );
                return Ok(CommandResult::Handled);
            };
            let payload = args.get(2..).unwrap_or(&[]).join(" ");
            if payload.trim().is_empty() {
                emit_command_output(app, "Payload cannot be empty.");
                return Ok(CommandResult::Handled);
            }
            let assessment = evaluate_trigger_triage(source, &payload);
            let mut out = render_trigger_triage_assessment(&assessment);
            if action == "queue" {
                match assessment.decision {
                    TriggerTriageDecision::Drop => {
                        out.push_str("\n\nqueue_action: dropped");
                    }
                    TriggerTriageDecision::Notify => {
                        out.push_str("\n\nqueue_action: notify-only (no agent run queued)");
                    }
                    TriggerTriageDecision::Escalate => {
                        let mut state = load_subconscious_state();
                        let id = format!(
                            "sc-{}",
                            uuid::Uuid::new_v4()
                                .simple()
                                .to_string()
                                .chars()
                                .take(8)
                                .collect::<String>()
                        );
                        state.tasks.push(SubconsciousTask {
                            id: id.clone(),
                            source: source.to_string(),
                            prompt: payload.trim().to_string(),
                            score: score_subconscious_task(&payload),
                            risk: "high".to_string(),
                            requires_approval: true,
                            status: "pending-approval".to_string(),
                            job_id: None,
                            created_at: chrono::Utc::now().to_rfc3339(),
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        });
                        save_subconscious_state(&state)?;
                        let _ = write!(
                            out,
                            "\n\nqueue_action: escalated to subconscious queue as {} (requires approval)",
                            id
                        );
                    }
                    TriggerTriageDecision::AgentRun => {
                        let job = queue_background_job(&app.state_root, payload.trim())?;
                        let _ = write!(
                            out,
                            "\n\nqueue_action: background job queued id={} status_file={}",
                            job.id,
                            job.status_path.display()
                        );
                    }
                }
            }
            emit_command_output(app, out);
        }
        _ => emit_command_output(
            app,
            "Usage: /triage [status|list|eval <source> <payload>|queue <source> <payload>|feedback <source> <outcome> <payload>]",
        ),
    }
    Ok(CommandResult::Handled)
}

use fs2::FileExt;

const SEALED_ALPHA_BACKGROUND_TASK_PLACEHOLDER: &str = "[sealed alpha-runtime-v1 task]";

type BackgroundStatusMap = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BackgroundChildIdentity {
    pid: u32,
    start_token: String,
    executable: String,
    claim_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedBackgroundProcess {
    start_token: String,
    executable: PathBuf,
}

trait BackgroundProcessInspector: Send + Sync {
    fn inspect(&self, pid: u32) -> Result<Option<ObservedBackgroundProcess>, String>;
}

struct SystemBackgroundProcessInspector;

impl BackgroundProcessInspector for SystemBackgroundProcessInspector {
    fn inspect(&self, raw_pid: u32) -> Result<Option<ObservedBackgroundProcess>, String> {
        use sysinfo::{Pid, ProcessesToUpdate, System};

        let pid = Pid::from_u32(raw_pid);
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        let Some(process) = system.process(pid) else {
            return Ok(None);
        };
        let executable = process
            .exe()
            .ok_or_else(|| "process executable is unavailable".to_string())?;
        let executable = std::fs::canonicalize(executable)
            .map_err(|error| format!("failed to canonicalize process executable: {error}"))?;
        Ok(Some(ObservedBackgroundProcess {
            start_token: process.start_time().to_string(),
            executable,
        }))
    }
}

trait BackgroundSignalSink: Send + Sync {
    fn terminate(&self, pid: u32) -> Result<(), String>;
}

struct SystemBackgroundSignalSink;

impl BackgroundSignalSink for SystemBackgroundSignalSink {
    #[cfg(unix)]
    fn terminate(&self, pid: u32) -> Result<(), String> {
        // SAFETY: callers validate the complete process identity immediately before signaling.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }

    #[cfg(not(unix))]
    fn terminate(&self, _pid: u32) -> Result<(), String> {
        Err("process termination is unsupported on this platform".to_string())
    }
}

#[derive(Clone)]
struct BackgroundExecutionDependencies {
    inspector: Arc<dyn BackgroundProcessInspector>,
    signals: Arc<dyn BackgroundSignalSink>,
    writer: Arc<dyn BackgroundStatusWriter>,
}

impl BackgroundExecutionDependencies {
    fn system() -> Self {
        Self {
            inspector: Arc::new(SystemBackgroundProcessInspector),
            signals: Arc::new(SystemBackgroundSignalSink),
            writer: Arc::new(AtomicBackgroundStatusWriter),
        }
    }
}

trait BackgroundStatusWriter: Send + Sync {
    fn replace(&self, path: &Path, map: &BackgroundStatusMap) -> std::io::Result<()>;
}

struct AtomicBackgroundStatusWriter;

impl BackgroundStatusWriter for AtomicBackgroundStatusWriter {
    fn replace(&self, path: &Path, map: &BackgroundStatusMap) -> std::io::Result<()> {
        atomic_replace_background_status(path, map)
    }
}

fn atomic_replace_background_status(
    path: &Path,
    map: &BackgroundStatusMap,
) -> std::io::Result<()> {
    let content = serde_json::to_vec_pretty(&serde_json::Value::Object(map.clone())).map_err(
        |error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
    )?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "background status path has no parent directory",
        )
    })?;
    let file_name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "background status path has no UTF-8 file name",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let temporary_path = parent.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut temporary = options.open(&temporary_path)?;
        std::io::Write::write_all(&mut temporary, &content)?;
        temporary.sync_all()?;
        drop(temporary);
        std::fs::rename(&temporary_path, path)?;
        sync_background_status_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(unix)]
fn sync_background_status_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_background_status_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn background_coordination_lock_path(
    status_path: &Path,
    purpose: &str,
) -> std::io::Result<PathBuf> {
    let file_name = status_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "background status path has no UTF-8 file name",
            )
        })?;
    Ok(status_path.with_file_name(format!(".{file_name}.{purpose}.lock")))
}

fn open_background_coordination_lock(
    status_path: &Path,
    purpose: &str,
) -> std::io::Result<std::fs::File> {
    let lock_path = background_coordination_lock_path(status_path, purpose)?;
    let parent = lock_path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "background lock path has no parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&lock_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

struct BackgroundSupervisorLease {
    _file: std::fs::File,
}

impl BackgroundSupervisorLease {
    fn try_acquire(status_path: &Path) -> std::io::Result<Option<Self>> {
        let file = open_background_coordination_lock(status_path, "supervisor")?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }
}

struct BackgroundStateLock {
    _file: std::fs::File,
}

impl BackgroundStateLock {
    fn acquire(status_path: &Path) -> std::io::Result<Self> {
        let file = open_background_coordination_lock(status_path, "state")?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }
}

#[derive(Clone, Debug)]
struct AlphaBackgroundBinding {
    metadata: serde_json::Value,
    id: Option<serde_json::Value>,
    task: serde_json::Value,
    log_path: Option<serde_json::Value>,
}

impl AlphaBackgroundBinding {
    fn capture(map: &BackgroundStatusMap) -> Result<Option<Self>, AgentError> {
        let Some(metadata) = map.get("alpha_runtime_v1") else {
            return Ok(None);
        };
        if !metadata.is_object()
            || map.get("task").and_then(|value| value.as_str())
                != Some(SEALED_ALPHA_BACKGROUND_TASK_PLACEHOLDER)
        {
            return Err(AgentError::Io(
                "Refusing malformed sealed Alpha background status.".to_string(),
            ));
        }
        Ok(Some(Self {
            metadata: metadata.clone(),
            id: map.get("id").cloned(),
            task: map
                .get("task")
                .cloned()
                .expect("sealed Alpha task was checked above"),
            log_path: map.get("log_path").cloned(),
        }))
    }

    fn ensure_preserved(&self, map: &BackgroundStatusMap) -> Result<(), AgentError> {
        if map.get("alpha_runtime_v1") != Some(&self.metadata)
            || map.get("id") != self.id.as_ref()
            || map.get("task") != Some(&self.task)
            || map.get("log_path") != self.log_path.as_ref()
        {
            return Err(AgentError::Io(
                "Refusing mutation that changes immutable sealed Alpha job binding.".to_string(),
            ));
        }
        Ok(())
    }
}

fn read_background_status_map(path: &Path) -> Result<BackgroundStatusMap, AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        AgentError::Io(format!(
            "Failed to read background status {}: {}",
            path.display(),
            error
        ))
    })?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|_| AgentError::Io("Background status is not valid JSON.".to_string()))?
        .as_object()
        .cloned()
        .ok_or_else(|| AgentError::Io("Background status is not a JSON object.".to_string()))
}

fn read_background_status_map_locked(path: &Path) -> Result<BackgroundStatusMap, AgentError> {
    let _state_lock = BackgroundStateLock::acquire(path)
        .map_err(|error| AgentError::Io(format!("Failed to lock background status: {error}")))?;
    read_background_status_map(path)
}

fn mutate_background_status<R>(
    path: &Path,
    writer: &dyn BackgroundStatusWriter,
    mutation: impl FnOnce(&mut BackgroundStatusMap) -> Result<(R, bool), AgentError>,
) -> Result<R, AgentError> {
    let _state_lock = BackgroundStateLock::acquire(path)
        .map_err(|error| AgentError::Io(format!("Failed to lock background status: {error}")))?;
    let mut map = read_background_status_map(path)?;
    let alpha_binding = AlphaBackgroundBinding::capture(&map)?;
    let (result, changed) = mutation(&mut map)?;
    if changed {
        if let Some(binding) = alpha_binding {
            binding.ensure_preserved(&map)?;
        }
        writer
            .replace(path, &map)
            .map_err(|error| AgentError::Io(format!("Failed to replace background status: {error}")))?;
    }
    Ok(result)
}

fn background_status_name(map: &BackgroundStatusMap) -> String {
    map.get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase()
}

fn is_background_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "canceled" | "outcome_unknown")
}

fn status_claim_id(map: &BackgroundStatusMap) -> Option<&str> {
    map.get("claim_id").and_then(|value| value.as_str())
}

fn map_child_identity(map: &BackgroundStatusMap) -> Result<BackgroundChildIdentity, ()> {
    let identity = map
        .get("child_identity")
        .cloned()
        .ok_or(())
        .and_then(|value| serde_json::from_value::<BackgroundChildIdentity>(value).map_err(|_| ()))?;
    if identity.pid == 0
        || identity.start_token.trim().is_empty()
        || identity.executable.trim().is_empty()
        || identity.claim_id.trim().is_empty()
        || status_claim_id(map) != Some(identity.claim_id.as_str())
        || map
            .get("pid")
            .and_then(|value| value.as_u64())
            .and_then(|pid| u32::try_from(pid).ok())
            != Some(identity.pid)
    {
        return Err(());
    }
    Ok(identity)
}

enum BackgroundIdentityCheck {
    Exact,
    Absent,
    Mismatch,
    Unverifiable,
}

fn check_background_child_identity(
    inspector: &dyn BackgroundProcessInspector,
    identity: &BackgroundChildIdentity,
) -> BackgroundIdentityCheck {
    match inspector.inspect(identity.pid) {
        Ok(Some(observed))
            if observed.start_token == identity.start_token
                && observed.executable.to_string_lossy() == identity.executable =>
        {
            BackgroundIdentityCheck::Exact
        }
        Ok(Some(_)) => BackgroundIdentityCheck::Mismatch,
        Ok(None) => BackgroundIdentityCheck::Absent,
        Err(_) => BackgroundIdentityCheck::Unverifiable,
    }
}

fn canonical_background_executable(path: &Path) -> Result<String, String> {
    std::fs::canonicalize(path)
        .map_err(|error| format!("failed to canonicalize executable: {error}"))?
        .into_os_string()
        .into_string()
        .map_err(|_| "canonical executable path is not UTF-8".to_string())
}

fn capture_background_child_identity(
    inspector: &dyn BackgroundProcessInspector,
    pid: u32,
    executable: &Path,
    claim_id: &str,
) -> Result<BackgroundChildIdentity, String> {
    let expected_executable = canonical_background_executable(executable)?;
    let observed = inspector
        .inspect(pid)?
        .ok_or_else(|| "child exited before its identity could be captured".to_string())?;
    let observed_executable = observed
        .executable
        .into_os_string()
        .into_string()
        .map_err(|_| "observed child executable path is not UTF-8".to_string())?;
    if observed.start_token.trim().is_empty() || observed_executable != expected_executable {
        return Err("child process identity did not match the spawned executable".to_string());
    }
    Ok(BackgroundChildIdentity {
        pid,
        start_token: observed.start_token,
        executable: expected_executable,
        claim_id: claim_id.to_string(),
    })
}
fn queue_background_job(state_root: &Path, task: &str) -> Result<QueuedBackgroundJob, AgentError> {
    let task = task.trim();
    if task.is_empty() {
        return Err(AgentError::Config(
            "Background task cannot be empty.".to_string(),
        ));
    }
    let job_id = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        uuid::Uuid::new_v4().simple()
    );
    let jobs_dir = state_root.join("background_jobs");
    std::fs::create_dir_all(&jobs_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create background job directory {}: {}",
            jobs_dir.display(),
            e
        ))
    })?;
    let status_path = jobs_dir.join(format!("{}.json", job_id));
    let log_path = jobs_dir.join(format!("{}.log", job_id));

    let status = serde_json::json!({
        "id": job_id,
        "task": task,
        "status": "queued",
        "attempts": 0,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "started_at": serde_json::Value::Null,
        "finished_at": serde_json::Value::Null,
        "exit_code": serde_json::Value::Null,
        "log_path": log_path,
    });
    let status_map = status
        .as_object()
        .cloned()
        .ok_or_else(|| AgentError::Io("Failed to encode background status.".to_string()))?;
    atomic_replace_background_status(&status_path, &status_map)
        .map_err(|error| AgentError::Io(format!("Failed to write background status: {error}")))?;

    schedule_background_job_execution(
        state_root.to_path_buf(),
        status_path.clone(),
        log_path.clone(),
        task.to_string(),
    );
    Ok(QueuedBackgroundJob {
        id: status["id"].as_str().unwrap_or("unknown").to_string(),
        task: task.to_string(),
        status_path,
        log_path,
    })
}

fn handle_background_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Usage: /background <message>\n\
             - /background status|list\n\
             - /background tail <job-id> [N]\n\
             - /background stop <job-id>\n\
             - /background event <source> <payload>\n\
             Queues a task to run in the background while you continue chatting.",
        );
        return Ok(CommandResult::Handled);
    }
    let sub = args[0].trim().to_ascii_lowercase();
    if sub == "status" || sub == "list" {
        emit_command_output(app, render_background_status(&app.state_root, 12));
        return Ok(CommandResult::Handled);
    }
    if sub == "tail" || sub == "log" || sub == "logs" || sub == "show" {
        let limit = args
            .get(2)
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(80)
            .clamp(5, 800);
        let requested_id = args
            .get(1)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                collect_background_jobs(&app.state_root, 1)
                    .into_iter()
                    .next()
                    .map(|row| row.id)
            });
        let Some(id_or_prefix) = requested_id else {
            emit_command_output(
                app,
                "Usage: /background tail <job-id> [N]\nNo jobs available yet.",
            );
            return Ok(CommandResult::Handled);
        };
        let Some(job) = resolve_background_job(&app.state_root, &id_or_prefix) else {
            emit_command_output(
                app,
                format!(
                    "Background job '{}' not found. Use `/background status`.",
                    id_or_prefix
                ),
            );
            return Ok(CommandResult::Handled);
        };
        let tail = if job.log_path.exists() {
            tail_file_lines(&job.log_path, limit)?
        } else {
            "(log file does not exist yet)".to_string()
        };
        emit_command_output(
            app,
            format!(
                "Background job\nid: {}\nstatus: {}\nattempts: {}\ncreated_at: {}\nstarted_at: {}\nfinished_at: {}\nstatus_file: {}\nlog_file: {}\n\n--- log tail ({}) ---\n{}",
                job.id,
                job.status,
                job.attempts,
                if job.created_at.is_empty() { "(n/a)" } else { job.created_at.as_str() },
                if job.started_at.is_empty() { "(n/a)" } else { job.started_at.as_str() },
                if job.finished_at.is_empty() { "(n/a)" } else { job.finished_at.as_str() },
                job.status_path.display(),
                job.log_path.display(),
                limit,
                if tail.trim().is_empty() { "(empty)" } else { tail.trim_end() }
            ),
        );
        return Ok(CommandResult::Handled);
    }
    if sub == "stop" || sub == "cancel" || sub == "kill" {
        let requested_id = args
            .get(1)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                collect_background_jobs(&app.state_root, 200).into_iter().find_map(|job| {
                    if matches!(job.status.as_str(), "running" | "queued") {
                        Some(job.id)
                    } else {
                        None
                    }
                })
            });
        let Some(id_or_prefix) = requested_id else {
            emit_command_output(
                app,
                "Usage: /background stop <job-id>\nNo running/queued jobs found.",
            );
            return Ok(CommandResult::Handled);
        };
        emit_command_output(app, terminate_background_job(&app.state_root, &id_or_prefix)?);
        return Ok(CommandResult::Handled);
    }
    if sub == "event" {
        let Some(source) = args.get(1).copied() else {
            emit_command_output(app, "Usage: /background event <source> <payload>");
            return Ok(CommandResult::Handled);
        };
        let payload = args.get(2..).unwrap_or(&[]).join(" ");
        if payload.trim().is_empty() {
            emit_command_output(app, "Usage: /background event <source> <payload>");
            return Ok(CommandResult::Handled);
        }
        let triage_args = vec!["queue", source];
        let mut merged = triage_args;
        let payload_parts: Vec<String> =
            payload.split_whitespace().map(|s| s.to_string()).collect();
        let payload_refs: Vec<&str> = payload_parts.iter().map(String::as_str).collect();
        merged.extend(payload_refs);
        return handle_trigger_triage_command(app, &merged);
    }
    let job = queue_background_job(&app.state_root, &args.join(" "))?;
    emit_command_output(
        app,
        format!(
            "[Background task queued: \"{}\"]\nJob ID: {}\nStatus: {}\nLogs:   {}\nThis task runs in a detached `hermes chat --query-stdin` process.",
            job.task,
            job.id,
            job.status_path.display(),
            job.log_path.display()
        ),
    );
    Ok(CommandResult::Handled)
}

fn apply_background_terminal_state(
    map: &mut BackgroundStatusMap,
    status: &str,
    terminal_reason: &str,
    exit_code: Option<i32>,
    error: Option<&str>,
) {
    map.insert(
        "status".to_string(),
        serde_json::Value::String(status.to_string()),
    );
    map.insert(
        "finished_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    map.insert(
        "terminal_reason".to_string(),
        serde_json::Value::String(terminal_reason.to_string()),
    );
    if let Some(exit_code) = exit_code {
        map.insert("exit_code".to_string(), serde_json::json!(exit_code));
    }
    if let Some(error) = error {
        map.insert(
            "error".to_string(),
            serde_json::Value::String(error.to_string()),
        );
    } else {
        map.remove("error");
    }
    map.insert("pid".to_string(), serde_json::Value::Null);
    map.insert(
        "execution_phase".to_string(),
        serde_json::Value::String("terminal".to_string()),
    );
}

fn transition_running_to_terminal(
    status_path: &Path,
    writer: &dyn BackgroundStatusWriter,
    expected_claim_id: &str,
    terminal_status: &str,
    terminal_reason: &str,
    exit_code: Option<i32>,
    error: Option<&str>,
) -> Result<bool, AgentError> {
    if !is_background_terminal_status(terminal_status) {
        return Err(AgentError::Io(
            "Background terminal transition requires a terminal status.".to_string(),
        ));
    }
    mutate_background_status(status_path, writer, |map| {
        if background_status_name(map) != "running"
            || status_claim_id(map) != Some(expected_claim_id)
        {
            return Ok((false, false));
        }
        apply_background_terminal_state(
            map,
            terminal_status,
            terminal_reason,
            exit_code,
            error,
        );
        Ok((true, true))
    })
}

fn transition_running_to_outcome_unknown(
    status_path: &Path,
    writer: &dyn BackgroundStatusWriter,
    expected_claim_id: Option<&str>,
    terminal_reason: &str,
) -> Result<bool, AgentError> {
    mutate_background_status(status_path, writer, |map| {
        if background_status_name(map) != "running"
            || expected_claim_id.is_some_and(|claim_id| status_claim_id(map) != Some(claim_id))
        {
            return Ok((false, false));
        }
        apply_background_terminal_state(
            map,
            "outcome_unknown",
            terminal_reason,
            None,
            Some("background child outcome cannot be established"),
        );
        Ok((true, true))
    })
}

enum BackgroundStopOutcome {
    AlreadyTerminal(String),
    CanceledQueued,
    CanceledStarting,
    CanceledRunning(u32),
    IdentityRefused,
    SignalFailed,
    UnsupportedState(String),
}

fn background_job_is_starting_without_identity(map: &BackgroundStatusMap) -> bool {
    map.get("execution_phase").and_then(|value| value.as_str()) == Some("claiming")
        && map.get("pid").is_some_and(serde_json::Value::is_null)
        && map
            .get("child_identity")
            .is_some_and(serde_json::Value::is_null)
}

fn terminate_background_job(state_root: &Path, id_or_prefix: &str) -> Result<String, AgentError> {
    let dependencies = BackgroundExecutionDependencies::system();
    terminate_background_job_with_dependencies(state_root, id_or_prefix, &dependencies)
}

fn terminate_background_job_with_dependencies(
    state_root: &Path,
    id_or_prefix: &str,
    dependencies: &BackgroundExecutionDependencies,
) -> Result<String, AgentError> {
    let Some(job) = resolve_background_job(state_root, id_or_prefix) else {
        return Ok(format!(
            "Background job '{}' not found. Use `/background status`.",
            id_or_prefix
        ));
    };
    let outcome = mutate_background_status(&job.status_path, dependencies.writer.as_ref(), |map| {
        let status = background_status_name(map);
        if is_background_terminal_status(&status) {
            return Ok((BackgroundStopOutcome::AlreadyTerminal(status), false));
        }
        if status == "queued" {
            apply_background_terminal_state(
                map,
                "canceled",
                "operator_canceled",
                None,
                Some("canceled by operator"),
            );
            return Ok((BackgroundStopOutcome::CanceledQueued, true));
        }
        if status != "running" {
            return Ok((BackgroundStopOutcome::UnsupportedState(status), false));
        }

        let identity = match map_child_identity(map) {
            Ok(identity) => identity,
            Err(()) if background_job_is_starting_without_identity(map) => {
                apply_background_terminal_state(
                    map,
                    "canceled",
                    "operator_canceled",
                    None,
                    Some("canceled by operator before child input"),
                );
                return Ok((BackgroundStopOutcome::CanceledStarting, true));
            }
            Err(()) => {
                apply_background_terminal_state(
                    map,
                    "outcome_unknown",
                    "malformed_child_identity",
                    None,
                    Some("background child identity is malformed"),
                );
                return Ok((BackgroundStopOutcome::IdentityRefused, true));
            }
        };
        match check_background_child_identity(dependencies.inspector.as_ref(), &identity) {
            BackgroundIdentityCheck::Exact => {
                if dependencies.signals.terminate(identity.pid).is_err() {
                    return Ok((BackgroundStopOutcome::SignalFailed, false));
                }
                apply_background_terminal_state(
                    map,
                    "canceled",
                    "operator_canceled",
                    None,
                    Some("canceled by operator"),
                );
                Ok((BackgroundStopOutcome::CanceledRunning(identity.pid), true))
            }
            BackgroundIdentityCheck::Absent => {
                apply_background_terminal_state(
                    map,
                    "outcome_unknown",
                    "child_identity_absent",
                    None,
                    Some("background child was absent before operator cancellation"),
                );
                Ok((BackgroundStopOutcome::IdentityRefused, true))
            }
            BackgroundIdentityCheck::Mismatch => {
                apply_background_terminal_state(
                    map,
                    "outcome_unknown",
                    "child_identity_mismatch",
                    None,
                    Some("background child identity no longer matched"),
                );
                Ok((BackgroundStopOutcome::IdentityRefused, true))
            }
            BackgroundIdentityCheck::Unverifiable => {
                apply_background_terminal_state(
                    map,
                    "outcome_unknown",
                    "child_identity_unverifiable",
                    None,
                    Some("background child identity could not be verified"),
                );
                Ok((BackgroundStopOutcome::IdentityRefused, true))
            }
        }
    })?;

    let message = match outcome {
        BackgroundStopOutcome::AlreadyTerminal(status) => {
            format!("Background job {} already {}.", job.id, status)
        }
        BackgroundStopOutcome::CanceledQueued => {
            format!("Canceled queued background job {}.", job.id)
        }
        BackgroundStopOutcome::CanceledStarting => format!(
            "Canceled background job {} before child input was delivered.",
            job.id
        ),
        BackgroundStopOutcome::CanceledRunning(pid) => format!(
            "Canceled background job {}. Sent SIGTERM to pid {} after exact child identity validation.",
            job.id, pid
        ),
        BackgroundStopOutcome::IdentityRefused => format!(
            "Refused SIGTERM for background job {} because its child identity could not be proven; status is outcome_unknown.",
            job.id
        ),
        BackgroundStopOutcome::SignalFailed => format!(
            "Failed to signal background job {}; status was left unchanged.",
            job.id
        ),
        BackgroundStopOutcome::UnsupportedState(status) => {
            format!("Background job {} cannot be canceled from status {}.", job.id, status)
        }
    };
    Ok(format!("{message}\nStatus file: {}", job.status_path.display()))
}

#[derive(Clone)]
struct BackgroundClaim {
    claim_id: String,
    started_at: String,
    status_snapshot: BackgroundStatusMap,
}

fn claim_queued_background_job(
    status_path: &Path,
    writer: &dyn BackgroundStatusWriter,
) -> Result<Option<BackgroundClaim>, AgentError> {
    mutate_background_status(status_path, writer, |queued| {
        if background_status_name(queued) != "queued" {
            return Ok((None, false));
        }
        let claim_id = uuid::Uuid::new_v4().to_string();
        let started_at = chrono::Utc::now().to_rfc3339();
        let attempts = queued
            .get("attempts")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .saturating_add(1);
        queued.insert(
            "status".to_string(),
            serde_json::Value::String("running".to_string()),
        );
        queued.insert(
            "execution_phase".to_string(),
            serde_json::Value::String("claiming".to_string()),
        );
        queued.insert(
            "claim_id".to_string(),
            serde_json::Value::String(claim_id.clone()),
        );
        queued.insert(
            "started_at".to_string(),
            serde_json::Value::String(started_at.clone()),
        );
        queued.insert("attempts".to_string(), serde_json::json!(attempts));
        queued.insert("pid".to_string(), serde_json::Value::Null);
        queued.insert("child_identity".to_string(), serde_json::Value::Null);
        queued.insert("finished_at".to_string(), serde_json::Value::Null);
        queued.insert("exit_code".to_string(), serde_json::Value::Null);
        queued.remove("terminal_reason");
        queued.remove("error");
        Ok((
            Some(BackgroundClaim {
                claim_id,
                started_at,
                status_snapshot: queued.clone(),
            }),
            true,
        ))
    })
}

fn running_claim_is_current(status_path: &Path, claim_id: &str) -> Result<bool, AgentError> {
    let map = read_background_status_map_locked(status_path)?;
    Ok(background_status_name(&map) == "running" && status_claim_id(&map) == Some(claim_id))
}

fn attach_background_child_identity(
    status_path: &Path,
    writer: &dyn BackgroundStatusWriter,
    claim_id: &str,
    identity: &BackgroundChildIdentity,
) -> Result<bool, AgentError> {
    if identity.claim_id != claim_id {
        return Ok(false);
    }
    let identity_value = serde_json::to_value(identity)
        .map_err(|_| AgentError::Io("Failed to encode child identity.".to_string()))?;
    mutate_background_status(status_path, writer, |map| {
        if background_status_name(map) != "running" || status_claim_id(map) != Some(claim_id) {
            return Ok((false, false));
        }
        map.insert("pid".to_string(), serde_json::json!(identity.pid));
        map.insert("child_identity".to_string(), identity_value);
        map.insert(
            "execution_phase".to_string(),
            serde_json::Value::String("running".to_string()),
        );
        Ok((true, true))
    })
}

#[derive(Clone)]
struct BackgroundLaunch {
    state_root: PathBuf,
    status_path: PathBuf,
    log_path: PathBuf,
    task: String,
    dependencies: BackgroundExecutionDependencies,
}

struct BackgroundChildSpawnPlan<'a> {
    executable: &'a Path,
    state_root: &'a Path,
    stdin: &'a [u8],
}

impl<'a> BackgroundChildSpawnPlan<'a> {
    fn arguments(&self) -> [&'static str; 2] {
        ["chat", "--query-stdin"]
    }

    fn environment(&self) -> [(&'static str, &'a std::ffi::OsStr); 1] {
        [("HERMES_HOME", self.state_root.as_os_str())]
    }
}

fn background_child_spawn_plan<'a>(
    executable: &'a Path,
    state_root: &'a Path,
    task: &'a str,
) -> BackgroundChildSpawnPlan<'a> {
    BackgroundChildSpawnPlan {
        executable,
        state_root,
        stdin: task.as_bytes(),
    }
}

fn background_child_command(plan: &BackgroundChildSpawnPlan<'_>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(plan.executable);
    command
        .args(plan.arguments())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.suppress_windows_console();
    // The lease is held through wait, and the child does not outlive a dropped supervisor.
    command.kill_on_drop(true);
    for (key, value) in plan.environment() {
        command.env(key, value);
    }
    command
}

fn schedule_background_job_execution(
    state_root: PathBuf,
    status_path: PathBuf,
    log_path: PathBuf,
    task: String,
) {
    let schedule_status_path = status_path.clone();
    if schedule_background_job_with_dependencies(
        state_root,
        status_path,
        log_path,
        task,
        BackgroundExecutionDependencies::system(),
    )
    .is_err()
    {
        tracing::warn!(
            status_path = %schedule_status_path.display(),
            "background job remains queued after a scheduling failure and will be retried by recovery"
        );
    }
}

fn schedule_background_job_with_dependencies(
    state_root: PathBuf,
    status_path: PathBuf,
    log_path: PathBuf,
    task: String,
    dependencies: BackgroundExecutionDependencies,
) -> Result<bool, AgentError> {
    let Some(supervisor_lease) = BackgroundSupervisorLease::try_acquire(&status_path)
        .map_err(|error| AgentError::Io(format!("Failed to acquire background supervisor lease: {error}")))?
    else {
        return Ok(false);
    };
    spawn_background_launch(
        BackgroundLaunch {
            state_root,
            status_path,
            log_path,
            task,
            dependencies,
        },
        supervisor_lease,
    );
    Ok(true)
}

fn spawn_background_launch(launch: BackgroundLaunch, supervisor_lease: BackgroundSupervisorLease) {
    tokio::spawn(async move {
        execute_background_launch(launch, supervisor_lease).await;
    });
}

async fn refuse_unidentified_background_child(
    mut child: tokio::process::Child,
    launch: &BackgroundLaunch,
    claim: &BackgroundClaim,
) {
    let _ = child.kill().await;
    let _ = child.wait().await;
    let _ = transition_running_to_terminal(
        &launch.status_path,
        launch.dependencies.writer.as_ref(),
        &claim.claim_id,
        "outcome_unknown",
        "child_identity_capture_failed",
        None,
        Some("background child identity could not be captured safely"),
    );
}

async fn write_background_child_stdin(
    child: &mut tokio::process::Child,
    query: &[u8],
) -> Result<(), ()> {
    use tokio::io::AsyncWriteExt as _;

    let Some(mut stdin) = child.stdin.take() else {
        return Err(());
    };
    stdin.write_all(query).await.map_err(|_| ())?;
    stdin.shutdown().await.map_err(|_| ())
}

async fn refuse_background_child_stdin_failure(
    mut child: tokio::process::Child,
    launch: &BackgroundLaunch,
    claim: &BackgroundClaim,
) {
    let _ = child.kill().await;
    let _ = child.wait().await;
    let _ = transition_running_to_terminal(
        &launch.status_path,
        launch.dependencies.writer.as_ref(),
        &claim.claim_id,
        "outcome_unknown",
        "stdin_write_failed",
        None,
        Some("failed to deliver the background query to the child"),
    );
}

fn render_background_completion_log(
    status_snapshot: &BackgroundStatusMap,
    started_at: &str,
    finished_at: &str,
    exit_code: i32,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let alpha_job = status_snapshot.contains_key("alpha_runtime_v1");
    let (task, stdout, stderr) = if alpha_job {
        (
            SEALED_ALPHA_BACKGROUND_TASK_PLACEHOLDER.to_string(),
            "[sealed Alpha child output withheld]".to_string(),
            "[sealed Alpha child diagnostics withheld]".to_string(),
        )
    } else {
        (
            status_snapshot
                .get("task")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            String::from_utf8_lossy(stdout).into_owned(),
            String::from_utf8_lossy(stderr).into_owned(),
        )
    };
    format!(
        "task: {task}\nstarted_at: {started_at}\nfinished_at: {finished_at}\nexit_code: {exit_code}\n\n[stdout]\n{stdout}\n\n[stderr]\n{stderr}\n"
    )
}

async fn execute_background_launch(
    launch: BackgroundLaunch,
    _supervisor_lease: BackgroundSupervisorLease,
) {
    let claim = match claim_queued_background_job(&launch.status_path, launch.dependencies.writer.as_ref()) {
        Ok(Some(claim)) => claim,
        Ok(None) => return,
        Err(_) => {
            tracing::warn!(
                status_path = %launch.status_path.display(),
                "background job remains durable after a claim failure and will be retried by recovery"
            );
            return;
        }
    };
    if !running_claim_is_current(&launch.status_path, &claim.claim_id).unwrap_or(false) {
        return;
    }

    let executable = match std::env::current_exe()
        .map_err(|error| error.to_string())
        .and_then(|path| {
            canonical_background_executable(&path)?;
            Ok(path)
        }) {
        Ok(path) => path,
        Err(_) => {
            let _ = transition_running_to_terminal(
                &launch.status_path,
                launch.dependencies.writer.as_ref(),
                &claim.claim_id,
                "failed",
                "current_exe_unavailable",
                None,
                Some("unable to determine the background executable"),
            );
            return;
        }
    };

    let spawn_plan =
        background_child_spawn_plan(&executable, &launch.state_root, launch.task.as_str());
    let mut command = background_child_command(&spawn_plan);
    if !running_claim_is_current(&launch.status_path, &claim.claim_id).unwrap_or(false) {
        return;
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = transition_running_to_terminal(
                &launch.status_path,
                launch.dependencies.writer.as_ref(),
                &claim.claim_id,
                "failed",
                "spawn_failed",
                None,
                Some("failed to spawn background child"),
            );
            return;
        }
    };
    let Some(pid) = child.id() else {
        refuse_unidentified_background_child(child, &launch, &claim).await;
        return;
    };
    let identity = match capture_background_child_identity(
        launch.dependencies.inspector.as_ref(),
        pid,
        &executable,
        &claim.claim_id,
    ) {
        Ok(identity) => identity,
        Err(_) => {
            refuse_unidentified_background_child(child, &launch, &claim).await;
            return;
        }
    };
    match attach_background_child_identity(
        &launch.status_path,
        launch.dependencies.writer.as_ref(),
        &claim.claim_id,
        &identity,
    ) {
        Ok(true) => {}
        Ok(false) | Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return;
        }
    }
    if write_background_child_stdin(&mut child, spawn_plan.stdin)
        .await
        .is_err()
    {
        refuse_background_child_stdin_failure(child, &launch, &claim).await;
        return;
    }


    let output = child.wait_with_output().await;
    match output {
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let finished_at = chrono::Utc::now().to_rfc3339();
            let log = render_background_completion_log(
                &claim.status_snapshot,
                &claim.started_at,
                &finished_at,
                exit_code,
                &output.stdout,
                &output.stderr,
            );
            let _ = std::fs::write(&launch.log_path, log);
            let (terminal_status, terminal_reason) = if output.status.success() {
                ("completed", "child_exit_success")
            } else {
                ("failed", "child_exit_failure")
            };
            let _ = transition_running_to_terminal(
                &launch.status_path,
                launch.dependencies.writer.as_ref(),
                &claim.claim_id,
                terminal_status,
                terminal_reason,
                Some(exit_code),
                None,
            );
        }
        Err(_) => {
            let _ = transition_running_to_terminal(
                &launch.status_path,
                launch.dependencies.writer.as_ref(),
                &claim.claim_id,
                "failed",
                "child_wait_failed",
                None,
                Some("failed while waiting for background child output"),
            );
        }
    }
}

/// Start a certified AlphaRuntimeV1 durable job only after the runtime has
/// exact-unsealed the receipt-bound task. The job status itself never stores
/// plaintext task material.
fn schedule_alpha_runtime_v1_background_job(
    state_root: &Path,
    runtime: &RunRuntime,
    job: &hermes_agent::AlphaRuntimeV1BackgroundJob,
) -> Result<(), AgentError> {
    let dependencies = BackgroundExecutionDependencies::system();
    let Some(supervisor_lease) = BackgroundSupervisorLease::try_acquire(job.status_path())
        .map_err(|error| AgentError::Io(format!("Failed to acquire background supervisor lease: {error}")))?
    else {
        return Ok(());
    };
    let task = runtime
        .resolve_alpha_runtime_v1_background_job_task(job)
        .map_err(|_| AgentError::Io("sealed AlphaRuntimeV1 job rejected".to_string()))?;
    spawn_background_launch(
        BackgroundLaunch {
            state_root: state_root.to_path_buf(),
            status_path: job.status_path().to_path_buf(),
            log_path: job.log_path().to_path_buf(),
            task,
            dependencies,
        },
        supervisor_lease,
    );
    Ok(())
}

#[derive(Clone)]
struct BackgroundRunningMonitor {
    status_path: PathBuf,
    claim_id: Option<String>,
    identity: BackgroundChildIdentity,
    dependencies: BackgroundExecutionDependencies,
}

fn monitor_background_running_job_once(
    monitor: &BackgroundRunningMonitor,
) -> Result<bool, AgentError> {
    match check_background_child_identity(monitor.dependencies.inspector.as_ref(), &monitor.identity) {
        BackgroundIdentityCheck::Exact => Ok(true),
        BackgroundIdentityCheck::Absent | BackgroundIdentityCheck::Mismatch => {
            let _ = transition_running_to_outcome_unknown(
                &monitor.status_path,
                monitor.dependencies.writer.as_ref(),
                monitor.claim_id.as_deref(),
                "supervisor_process_death",
            )?;
            Ok(false)
        }
        BackgroundIdentityCheck::Unverifiable => {
            let _ = transition_running_to_outcome_unknown(
                &monitor.status_path,
                monitor.dependencies.writer.as_ref(),
                monitor.claim_id.as_deref(),
                "child_identity_unverifiable",
            )?;
            Ok(false)
        }
    }
}

async fn monitor_background_running_job(
    monitor: BackgroundRunningMonitor,
    _supervisor_lease: BackgroundSupervisorLease,
) {
    while monitor_background_running_job_once(&monitor).unwrap_or(false) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Clone)]
struct BackgroundRecoveryHooks {
    launch: Arc<dyn Fn(BackgroundLaunch, BackgroundSupervisorLease) + Send + Sync>,
    monitor: Arc<dyn Fn(BackgroundRunningMonitor, BackgroundSupervisorLease) + Send + Sync>,
}

impl BackgroundRecoveryHooks {
    fn production() -> Self {
        Self {
            launch: Arc::new(spawn_background_launch),
            monitor: Arc::new(|monitor, supervisor_lease| {
                tokio::spawn(async move {
                    monitor_background_running_job(monitor, supervisor_lease).await;
                });
            }),
        }
    }
}

fn recover_running_background_job(
    status_path: &Path,
    map: &BackgroundStatusMap,
    supervisor_lease: BackgroundSupervisorLease,
    dependencies: BackgroundExecutionDependencies,
    hooks: &BackgroundRecoveryHooks,
) -> bool {
    let claim_id = status_claim_id(map).map(ToString::to_string);
    let identity = match map_child_identity(map) {
        Ok(identity) => identity,
        Err(()) => {
            return transition_running_to_outcome_unknown(
                status_path,
                dependencies.writer.as_ref(),
                claim_id.as_deref(),
                "malformed_child_identity",
            )
            .unwrap_or(false);
        }
    };
    match check_background_child_identity(dependencies.inspector.as_ref(), &identity) {
        BackgroundIdentityCheck::Exact => {
            (hooks.monitor)(
                BackgroundRunningMonitor {
                    status_path: status_path.to_path_buf(),
                    claim_id,
                    identity,
                    dependencies,
                },
                supervisor_lease,
            );
            true
        }
        BackgroundIdentityCheck::Absent | BackgroundIdentityCheck::Mismatch => {
            transition_running_to_outcome_unknown(
                status_path,
                dependencies.writer.as_ref(),
                claim_id.as_deref(),
                "supervisor_process_death",
            )
            .unwrap_or(false)
        }
        BackgroundIdentityCheck::Unverifiable => transition_running_to_outcome_unknown(
            status_path,
            dependencies.writer.as_ref(),
            claim_id.as_deref(),
            "child_identity_unverifiable",
        )
        .unwrap_or(false),
    }
}

pub fn recover_queued_background_jobs(
    state_root: &Path,
    runtime: &RunRuntime,
    max_jobs: usize,
) -> usize {
    let dependencies = BackgroundExecutionDependencies::system();
    let hooks = BackgroundRecoveryHooks::production();
    recover_background_jobs_with_dependencies(
        state_root,
        Some(runtime),
        max_jobs,
        dependencies,
        &hooks,
    )
}

fn recover_background_jobs_with_dependencies(
    state_root: &Path,
    runtime: Option<&RunRuntime>,
    max_jobs: usize,
    dependencies: BackgroundExecutionDependencies,
    hooks: &BackgroundRecoveryHooks,
) -> usize {
    let jobs_dir = state_root.join("background_jobs");
    let Ok(entries) = std::fs::read_dir(&jobs_dir) else {
        return 0;
    };
    let mut recovered = 0usize;
    for entry in entries.filter_map(Result::ok) {
        if recovered >= max_jobs.max(1) {
            break;
        }
        let status_path = entry.path();
        if status_path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let supervisor_lease = match BackgroundSupervisorLease::try_acquire(&status_path) {
            Ok(Some(lease)) => lease,
            Ok(None) => continue,
            Err(_) => {
                tracing::warn!(
                    status_path = %status_path.display(),
                    "durable background job was left unchanged because its supervisor lease could not be inspected"
                );
                continue;
            }
        };
        let map = match read_background_status_map_locked(&status_path) {
            Ok(map) => map,
            Err(_) => continue,
        };
        match background_status_name(&map).as_str() {
            "running" => {
                let _ = recover_running_background_job(
                    &status_path,
                    &map,
                    supervisor_lease,
                    dependencies.clone(),
                    hooks,
                );
            }
            "queued" => {
                let (task, log_path) = if map.contains_key("alpha_runtime_v1") {
                    let alpha_job = match hermes_agent::AlphaRuntimeV1BackgroundJob::from_status_value(
                        state_root,
                        &status_path,
                        &serde_json::Value::Object(map.clone()),
                    ) {
                        Ok(Some(job)) => job,
                        Ok(None) | Err(_) => {
                            tracing::warn!(
                                status_path = %status_path.display(),
                                "refusing malformed sealed Alpha background job during recovery"
                            );
                            continue;
                        }
                    };
                    let Some(runtime) = runtime else {
                        continue;
                    };
                    let task = match runtime.resolve_alpha_runtime_v1_background_job_task(&alpha_job) {
                        Ok(task) => task,
                        Err(_) => {
                            tracing::warn!(
                                status_path = %status_path.display(),
                                "sealed Alpha background task remains queued because it could not be resolved"
                            );
                            continue;
                        }
                    };
                    (task, alpha_job.log_path().to_path_buf())
                } else {
                    let Some(task) = map
                        .get("task")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToString::to_string)
                    else {
                        tracing::warn!(
                            status_path = %status_path.display(),
                            "background job remains queued because its task is missing"
                        );
                        continue;
                    };
                    let log_path = map
                        .get("log_path")
                        .and_then(|value| value.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| status_path.with_extension("log"));
                    (task, log_path)
                };
                (hooks.launch)(
                    BackgroundLaunch {
                        state_root: state_root.to_path_buf(),
                        status_path,
                        log_path,
                        task,
                        dependencies: dependencies.clone(),
                    },
                    supervisor_lease,
                );
                recovered = recovered.saturating_add(1);
            }
            _ => {}
        }
    }
    recovered
}

fn read_json_map(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}


#[cfg(test)]
mod background_durable_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    struct FakeProcessInspector {
        observations: Mutex<VecDeque<Result<Option<ObservedBackgroundProcess>, String>>>,
    }

    impl FakeProcessInspector {
        fn new(observations: Vec<Result<Option<ObservedBackgroundProcess>, String>>) -> Self {
            Self {
                observations: Mutex::new(observations.into()),
            }
        }
    }

    impl BackgroundProcessInspector for FakeProcessInspector {
        fn inspect(&self, _pid: u32) -> Result<Option<ObservedBackgroundProcess>, String> {
            self.observations
                .lock()
                .expect("fake inspector lock")
                .pop_front()
                .unwrap_or(Ok(None))
        }
    }

    #[derive(Default)]
    struct FakeSignalSink {
        calls: Mutex<Vec<u32>>,
        fail: bool,
    }

    impl BackgroundSignalSink for FakeSignalSink {
        fn terminate(&self, pid: u32) -> Result<(), String> {
            self.calls.lock().expect("fake signal lock").push(pid);
            if self.fail {
                Err("signal failed".to_string())
            } else {
                Ok(())
            }
        }
    }

    struct FailingStatusWriter;

    impl BackgroundStatusWriter for FailingStatusWriter {
        fn replace(&self, _path: &Path, _map: &BackgroundStatusMap) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "injected atomic replacement failure",
            ))
        }
    }

    fn test_dependencies(
        observations: Vec<Result<Option<ObservedBackgroundProcess>, String>>,
        signals: Arc<FakeSignalSink>,
    ) -> BackgroundExecutionDependencies {
        BackgroundExecutionDependencies {
            inspector: Arc::new(FakeProcessInspector::new(observations)),
            signals,
            writer: Arc::new(AtomicBackgroundStatusWriter),
        }
    }

    fn write_test_status(
        root: &Path,
        id: &str,
        status: &str,
        task: &str,
        claim_id: Option<&str>,
        identity: Option<BackgroundChildIdentity>,
    ) -> PathBuf {
        let jobs = root.join("background_jobs");
        std::fs::create_dir_all(&jobs).expect("jobs directory");
        let status_path = jobs.join(format!("{id}.json"));
        let log_path = jobs.join(format!("{id}.log"));
        let mut map = BackgroundStatusMap::new();
        map.insert("id".to_string(), serde_json::Value::String(id.to_string()));
        map.insert("status".to_string(), serde_json::Value::String(status.to_string()));
        map.insert("task".to_string(), serde_json::Value::String(task.to_string()));
        map.insert("attempts".to_string(), serde_json::json!(1));
        map.insert(
            "created_at".to_string(),
            serde_json::Value::String("2026-07-20T00:00:00Z".to_string()),
        );
        map.insert("started_at".to_string(), serde_json::Value::Null);
        map.insert("finished_at".to_string(), serde_json::Value::Null);
        map.insert("exit_code".to_string(), serde_json::Value::Null);
        map.insert(
            "log_path".to_string(),
            serde_json::Value::String(log_path.display().to_string()),
        );
        if let Some(claim_id) = claim_id {
            map.insert(
                "claim_id".to_string(),
                serde_json::Value::String(claim_id.to_string()),
            );
        }
        if let Some(identity) = identity {
            map.insert("pid".to_string(), serde_json::json!(identity.pid));
            map.insert(
                "child_identity".to_string(),
                serde_json::to_value(identity).expect("identity json"),
            );
        } else {
            map.insert("pid".to_string(), serde_json::Value::Null);
            map.insert("child_identity".to_string(), serde_json::Value::Null);
        }
        atomic_replace_background_status(&status_path, &map).expect("status write");
        status_path
    }

    fn running_identity(pid: u32, start_token: &str, executable: &str) -> BackgroundChildIdentity {
        BackgroundChildIdentity {
            pid,
            start_token: start_token.to_string(),
            executable: executable.to_string(),
            claim_id: "claim-a".to_string(),
        }
    }

    fn observed(identity: &BackgroundChildIdentity) -> ObservedBackgroundProcess {
        ObservedBackgroundProcess {
            start_token: identity.start_token.clone(),
            executable: PathBuf::from(&identity.executable),
        }
    }

    fn no_op_hooks() -> BackgroundRecoveryHooks {
        BackgroundRecoveryHooks {
            launch: Arc::new(|_, _| {}),
            monitor: Arc::new(|_, _| {}),
        }
    }

    #[test]
    fn concurrent_queued_recovery_schedules_once() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().to_path_buf();
        let status_path = write_test_status(&root, "queued-once", "queued", "ordinary task", None, None);
        let signals = Arc::new(FakeSignalSink::default());
        let dependencies = test_dependencies(Vec::new(), signals);
        let scheduled = Arc::new(Mutex::new(0usize));
        let held_leases = Arc::new(Mutex::new(Vec::<BackgroundSupervisorLease>::new()));
        let hooks = BackgroundRecoveryHooks {
            launch: {
                let scheduled = Arc::clone(&scheduled);
                let held_leases = Arc::clone(&held_leases);
                Arc::new(move |_, lease| {
                    *scheduled.lock().expect("schedule count") += 1;
                    held_leases.lock().expect("lease storage").push(lease);
                })
            },
            monitor: Arc::new(|_, _| {}),
        };

        let (first, second) = std::thread::scope(|scope| {
            let first_root = root.clone();
            let first_dependencies = dependencies.clone();
            let first_hooks = hooks.clone();
            let first = scope.spawn(move || {
                recover_background_jobs_with_dependencies(
                    &first_root,
                    None,
                    8,
                    first_dependencies,
                    &first_hooks,
                )
            });
            let second_root = root.clone();
            let second_dependencies = dependencies.clone();
            let second_hooks = hooks.clone();
            let second = scope.spawn(move || {
                recover_background_jobs_with_dependencies(
                    &second_root,
                    None,
                    8,
                    second_dependencies,
                    &second_hooks,
                )
            });
            (
                first.join().expect("first recovery"),
                second.join().expect("second recovery"),
            )
        });

        assert_eq!(first + second, 1);
        assert_eq!(*scheduled.lock().expect("schedule count"), 1);
        assert_eq!(background_status_name(&read_background_status_map(&status_path).unwrap()), "queued");
    }

    #[test]
    fn stale_running_absent_child_becomes_outcome_unknown() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let identity = running_identity(41, "start-a", "/tmp/hermes-child");
        let status_path = write_test_status(
            root,
            "stale-absent",
            "running",
            "ordinary task",
            Some("claim-a"),
            Some(identity),
        );
        let dependencies = test_dependencies(Vec::new(), Arc::new(FakeSignalSink::default()));
        let recovered = recover_background_jobs_with_dependencies(
            root,
            None,
            8,
            dependencies,
            &no_op_hooks(),
        );

        let map = read_background_status_map(&status_path).expect("status");
        assert_eq!(recovered, 0);
        assert_eq!(background_status_name(&map), "outcome_unknown");
        assert_eq!(
            map.get("terminal_reason").and_then(|value| value.as_str()),
            Some("supervisor_process_death")
        );
    }

    #[test]
    fn malformed_running_identity_becomes_unknown_without_launching() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let status_path = write_test_status(
            root,
            "malformed-running",
            "running",
            "ordinary task",
            Some("claim-a"),
            None,
        );
        let recovered = recover_background_jobs_with_dependencies(
            root,
            None,
            8,
            test_dependencies(Vec::new(), Arc::new(FakeSignalSink::default())),
            &no_op_hooks(),
        );

        let map = read_background_status_map(&status_path).expect("status");
        assert_eq!(recovered, 0);
        assert_eq!(background_status_name(&map), "outcome_unknown");
        assert_eq!(
            map.get("terminal_reason").and_then(|value| value.as_str()),
            Some("malformed_child_identity")
        );
    }

    #[test]
    fn exact_live_running_child_is_monitored_without_resubmission() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let identity = running_identity(42, "start-b", "/tmp/hermes-child");
        let status_path = write_test_status(
            root,
            "live-monitor",
            "running",
            "ordinary task",
            Some("claim-a"),
            Some(identity.clone()),
        );
        let dependencies = test_dependencies(
            vec![Ok(Some(observed(&identity))), Ok(None)],
            Arc::new(FakeSignalSink::default()),
        );
        let launches = Arc::new(Mutex::new(0usize));
        let monitors = Arc::new(Mutex::new(0usize));
        let hooks = BackgroundRecoveryHooks {
            launch: {
                let launches = Arc::clone(&launches);
                Arc::new(move |_, _| *launches.lock().expect("launch count") += 1)
            },
            monitor: {
                let monitors = Arc::clone(&monitors);
                Arc::new(move |monitor, _| {
                    *monitors.lock().expect("monitor count") += 1;
                    assert!(!monitor_background_running_job_once(&monitor).expect("monitor check"));
                })
            },
        };

        let recovered =
            recover_background_jobs_with_dependencies(root, None, 8, dependencies, &hooks);
        let map = read_background_status_map(&status_path).expect("status");
        assert_eq!(recovered, 0);
        assert_eq!(*launches.lock().expect("launch count"), 0);
        assert_eq!(*monitors.lock().expect("monitor count"), 1);
        assert_eq!(background_status_name(&map), "outcome_unknown");
        assert_eq!(
            map.get("terminal_reason").and_then(|value| value.as_str()),
            Some("supervisor_process_death")
        );
    }

    #[test]
    fn active_supervisor_lease_is_untouched_by_recovery() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let identity = running_identity(43, "start-c", "/tmp/hermes-child");
        let status_path = write_test_status(
            root,
            "active-supervisor",
            "running",
            "ordinary task",
            Some("claim-a"),
            Some(identity),
        );
        let _lease = BackgroundSupervisorLease::try_acquire(&status_path)
            .expect("lease")
            .expect("lease acquired");
        let recovered = recover_background_jobs_with_dependencies(
            root,
            None,
            8,
            test_dependencies(Vec::new(), Arc::new(FakeSignalSink::default())),
            &no_op_hooks(),
        );

        assert_eq!(recovered, 0);
        assert_eq!(
            background_status_name(&read_background_status_map(&status_path).expect("status")),
            "running"
        );
    }

    #[test]
    fn pid_reuse_mismatch_is_never_signaled() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let identity = running_identity(44, "old-start", "/tmp/hermes-child");
        let status_path = write_test_status(
            root,
            "pid-reuse",
            "running",
            "ordinary task",
            Some("claim-a"),
            Some(identity.clone()),
        );
        let signals = Arc::new(FakeSignalSink::default());
        let dependencies = test_dependencies(
            vec![Ok(Some(ObservedBackgroundProcess {
                start_token: "new-start".to_string(),
                executable: PathBuf::from(&identity.executable),
            }))],
            Arc::clone(&signals),
        );
        let message =
            terminate_background_job_with_dependencies(root, "pid-reuse", &dependencies).expect("stop");

        assert!(message.contains("Refused SIGTERM"));
        assert!(signals.calls.lock().expect("signal calls").is_empty());
        assert_eq!(
            background_status_name(&read_background_status_map(&status_path).expect("status")),
            "outcome_unknown"
        );
    }

    #[test]
    fn pending_claim_cancellation_never_signals_without_an_identity() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let status_path = write_test_status(
            root,
            "pending-cancel",
            "running",
            "ordinary task",
            Some("claim-a"),
            None,
        );
        let mut map = read_background_status_map(&status_path).expect("status");
        map.insert(
            "execution_phase".to_string(),
            serde_json::Value::String("claiming".to_string()),
        );
        atomic_replace_background_status(&status_path, &map).expect("claiming fixture");
        let signals = Arc::new(FakeSignalSink::default());
        let dependencies = test_dependencies(Vec::new(), Arc::clone(&signals));

        let message =
            terminate_background_job_with_dependencies(root, "pending-cancel", &dependencies)
                .expect("cancel");
        assert!(message.contains("before child input"));
        assert!(signals.calls.lock().expect("signal calls").is_empty());
        assert_eq!(
            background_status_name(&read_background_status_map(&status_path).expect("status")),
            "canceled"
        );
    }

    #[test]
    fn canceled_job_cannot_be_regressed_by_stale_waiter() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let identity = running_identity(45, "start-d", "/tmp/hermes-child");
        let status_path = write_test_status(
            root,
            "cancel-race",
            "running",
            "ordinary task",
            Some("claim-a"),
            Some(identity.clone()),
        );
        let signals = Arc::new(FakeSignalSink::default());
        let dependencies =
            test_dependencies(vec![Ok(Some(observed(&identity)))], Arc::clone(&signals));
        let _ = terminate_background_job_with_dependencies(root, "cancel-race", &dependencies)
            .expect("operator cancellation");
        assert!(signals.calls.lock().expect("signal calls").contains(&45));

        assert!(!transition_running_to_terminal(
            &status_path,
            dependencies.writer.as_ref(),
            "claim-a",
            "completed",
            "child_exit_success",
            Some(0),
            None,
        )
        .expect("stale waiter transition"));
        assert_eq!(
            background_status_name(&read_background_status_map(&status_path).expect("status")),
            "canceled"
        );
    }

    #[test]
    fn all_terminal_statuses_are_immutable_to_claim_waiters() {
        let temp = tempfile::tempdir().expect("temp root");
        let writer = AtomicBackgroundStatusWriter;
        for terminal in ["completed", "failed", "canceled", "outcome_unknown"] {
            let status_path = write_test_status(
                temp.path(),
                terminal,
                terminal,
                "ordinary task",
                Some("claim-a"),
                None,
            );
            assert!(!transition_running_to_terminal(
                &status_path,
                &writer,
                "claim-a",
                "completed",
                "child_exit_success",
                Some(0),
                None,
            )
            .expect("terminal transition check"));
            assert_eq!(
                background_status_name(&read_background_status_map(&status_path).expect("status")),
                terminal
            );
        }
    }

    #[test]
    fn failed_atomic_replacement_keeps_last_valid_status_json() {
        let temp = tempfile::tempdir().expect("temp root");
        let status_path =
            write_test_status(temp.path(), "atomic-failure", "queued", "ordinary task", None, None);
        let result = mutate_background_status(&status_path, &FailingStatusWriter, |map| {
            map.insert(
                "status".to_string(),
                serde_json::Value::String("running".to_string()),
            );
            Ok(((), true))
        });

        assert!(result.is_err());
        let raw = std::fs::read_to_string(&status_path).expect("preserved status bytes");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON remains");
        assert_eq!(parsed.get("status").and_then(|value| value.as_str()), Some("queued"));
    }

    #[test]
    fn status_rendering_enumerates_each_terminal_state() {
        let temp = tempfile::tempdir().expect("temp root");
        for status in [
            "queued",
            "running",
            "completed",
            "failed",
            "canceled",
            "outcome_unknown",
            "unknown",
        ] {
            write_test_status(temp.path(), status, status, "ordinary task", None, None);
        }

        assert_eq!(
            background_job_counts(temp.path()),
            BackgroundJobCounts {
                queued: 1,
                running: 1,
                completed: 1,
                failed: 1,
                canceled: 1,
                outcome_unknown: 2,
            }
        );
        assert!(
            render_background_status(temp.path(), 12).contains(
                "queued=1 running=1 completed=1 failed=1 canceled=1 outcome_unknown=2"
            )
        );
    }

    #[test]
    fn background_spawn_plan_keeps_query_out_of_argv_and_environment() {
        let sentinel = "sealed Alpha task sentinel";
        let plan = background_child_spawn_plan(
            Path::new("/private/background-child"),
            Path::new("/private/background-state"),
            sentinel,
        );

        assert!(!plan
            .arguments()
            .iter()
            .any(|argument| *argument == "--query" || *argument == sentinel));
        assert!(plan
            .environment()
            .iter()
            .all(|(_, value)| !value.to_string_lossy().contains(sentinel)));
        let command = background_child_command(&plan);
        assert!(command
            .as_std()
            .get_args()
            .all(|argument| !argument.to_string_lossy().contains(sentinel)));
        assert!(command.as_std().get_envs().all(|(_, value)| match value {
            Some(value) => !value.to_string_lossy().contains(sentinel),
            None => true,
        }));
        assert_eq!(plan.stdin, sentinel.as_bytes());
    }

    #[test]
    fn alpha_completion_log_redacts_task_and_child_diagnostics() {
        let secret = "sealed Alpha task sentinel";
        let mut status = BackgroundStatusMap::new();
        status.insert(
            "alpha_runtime_v1".to_string(),
            serde_json::json!({"schema": "hermes.alpha-runtime-v1.background-job/v1"}),
        );
        status.insert(
            "task".to_string(),
            serde_json::Value::String(SEALED_ALPHA_BACKGROUND_TASK_PLACEHOLDER.to_string()),
        );

        let log = render_background_completion_log(
            &status,
            "2026-07-20T00:00:00Z",
            "2026-07-20T00:01:00Z",
            0,
            secret.as_bytes(),
            secret.as_bytes(),
        );
        assert!(!log.contains(secret));
        assert!(log.contains(SEALED_ALPHA_BACKGROUND_TASK_PLACEHOLDER));
        assert!(log.contains("withheld"));
    }

    #[test]
    fn malformed_alpha_status_is_refused_without_plaintext_diagnostics() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path();
        let secret = "TOP SECRET sealed alpha task";
        let status_path = write_test_status(root, "malformed-alpha", "queued", secret, None, None);
        let mut map = read_background_status_map(&status_path).expect("status");
        map.insert("alpha_runtime_v1".to_string(), serde_json::json!({}));
        atomic_replace_background_status(&status_path, &map).expect("malformed fixture");

        let launches = Arc::new(Mutex::new(0usize));
        let hooks = BackgroundRecoveryHooks {
            launch: {
                let launches = Arc::clone(&launches);
                Arc::new(move |_, _| *launches.lock().expect("launch count") += 1)
            },
            monitor: Arc::new(|_, _| {}),
        };
        let recovered = recover_background_jobs_with_dependencies(
            root,
            None,
            8,
            test_dependencies(Vec::new(), Arc::new(FakeSignalSink::default())),
            &hooks,
        );
        let guard_error = AlphaBackgroundBinding::capture(&map)
            .expect_err("plaintext Alpha status is rejected")
            .to_string();

        assert_eq!(recovered, 0);
        assert_eq!(*launches.lock().expect("launch count"), 0);
        assert!(!guard_error.contains(secret));
        assert_eq!(
            read_background_status_map(&status_path)
                .expect("status")
                .get("task")
                .and_then(|value| value.as_str()),
            Some(secret)
        );
        assert!(!status_path.with_extension("log").exists());
    }

    #[test]
    fn alpha_binding_survives_background_state_mutation() {
        let temp = tempfile::tempdir().expect("temp root");
        let status_path = write_test_status(
            temp.path(),
            "sealed-alpha",
            "running",
            SEALED_ALPHA_BACKGROUND_TASK_PLACEHOLDER,
            Some("claim-a"),
            None,
        );
        let mut map = read_background_status_map(&status_path).expect("status");
        let metadata = serde_json::json!({"schema": "hermes.alpha-runtime-v1.background-job/v1"});
        map.insert("alpha_runtime_v1".to_string(), metadata.clone());
        atomic_replace_background_status(&status_path, &map).expect("alpha fixture");

        assert!(transition_running_to_terminal(
            &status_path,
            &AtomicBackgroundStatusWriter,
            "claim-a",
            "failed",
            "spawn_failed",
            None,
            Some("failed to spawn background child"),
        )
        .expect("transition"));
        let after = read_background_status_map(&status_path).expect("status");
        assert_eq!(after.get("alpha_runtime_v1"), Some(&metadata));
        assert_eq!(
            after.get("task").and_then(|value| value.as_str()),
            Some(SEALED_ALPHA_BACKGROUND_TASK_PLACEHOLDER)
        );
    }
}

fn handle_verbose_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let current = tracing::enabled!(tracing::Level::DEBUG);
    if current {
        emit_command_output(
            app,
            "Verbose mode: OFF (switching to info level)\n(Runtime log level changes require restart — use `hermes -v` for verbose)",
        );
    } else {
        emit_command_output(
            app,
            "Verbose mode: ON (switching to debug level)\n(Runtime log level changes require restart — use `hermes -v` for verbose)",
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_yolo_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let currently_required = app.config.approval.require_approval;
    let new_val = !currently_required;

    app.config = Arc::new({
        let mut cfg = (*app.config).clone();
        cfg.approval.require_approval = new_val;
        cfg
    });

    if !new_val {
        emit_command_output(
            app,
            "YOLO mode: ON — tool executions will not require approval.\nBe careful! The agent can now execute tools without confirmation.",
        );
    } else {
        emit_command_output(
            app,
            "YOLO mode: OFF — tool executions will require approval.",
        );
    }
    Ok(CommandResult::Handled)
}
