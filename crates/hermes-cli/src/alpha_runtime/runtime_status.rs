pub fn set_contextlattice_policy_mode(mode: &str) -> Result<ContextLatticePolicy, AgentError> {
    let mut policy = load_contextlattice_policy()?;
    match mode.trim().to_ascii_lowercase().as_str() {
        "max" | "strict" => {
            policy.preflight_required = true;
            policy.auto_context_pack_on_mission_start = true;
            policy.degradation_aware_planning = true;
            policy.readback_verification_required = true;
            policy.include_grounding_required = true;
            policy.include_retrieval_debug_for_execution = true;
            policy.broaden_scope_on_zero_hits = true;
            policy.scoped_recency_pass_before_finalize = true;
            policy.contradiction_check_across_layers = true;
            policy.numeric_fact_verbatim_copy = true;
            policy.objective_analytics_writeback_required = true;
            policy.required_project_scoping = true;
            policy.checkpoint_payload_requires_project_file_topic = true;
            policy.preferred_retrieval_mode = "deep".to_string();
            policy.deep_retry_budget_secs = vec![120, 180, 240];
            policy.regular_retry_budget_secs = vec![120, 180];
        }
        "balanced" => {
            policy.preflight_required = true;
            policy.auto_context_pack_on_mission_start = true;
            policy.degradation_aware_planning = true;
            policy.readback_verification_required = true;
            policy.include_grounding_required = true;
            policy.include_retrieval_debug_for_execution = true;
            policy.broaden_scope_on_zero_hits = true;
            policy.scoped_recency_pass_before_finalize = true;
            policy.contradiction_check_across_layers = true;
            policy.numeric_fact_verbatim_copy = true;
            policy.objective_analytics_writeback_required = true;
            policy.required_project_scoping = true;
            policy.checkpoint_payload_requires_project_file_topic = true;
            policy.preferred_retrieval_mode = "balanced".to_string();
            policy.deep_retry_budget_secs = vec![90, 120, 180];
            policy.regular_retry_budget_secs = vec![90, 120];
        }
        "speed" | "fast" => {
            policy.preflight_required = true;
            policy.auto_context_pack_on_mission_start = true;
            policy.degradation_aware_planning = true;
            policy.readback_verification_required = true;
            policy.include_grounding_required = true;
            policy.include_retrieval_debug_for_execution = false;
            policy.broaden_scope_on_zero_hits = true;
            policy.scoped_recency_pass_before_finalize = true;
            policy.contradiction_check_across_layers = true;
            policy.numeric_fact_verbatim_copy = true;
            policy.objective_analytics_writeback_required = true;
            policy.required_project_scoping = true;
            policy.checkpoint_payload_requires_project_file_topic = true;
            policy.preferred_retrieval_mode = "fast".to_string();
            policy.deep_retry_budget_secs = vec![60, 90, 120];
            policy.regular_retry_budget_secs = vec![60, 90];
        }
        _ => {
            return Err(AgentError::Config(
                "unknown contextlattice policy mode; expected one of: max|strict|balanced|fast|speed"
                    .to_string(),
            ));
        }
    }
    set_contextlattice_policy(policy)
}

pub fn enqueue_loop_event(
    loop_id: &str,
    event_type: &str,
    payload: &str,
) -> Result<LoopQueueEvent, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let source = format!(
        "{}|{}|{}|{}",
        loop_id.trim(),
        event_type.trim(),
        payload.trim(),
        now_rfc3339()
    );
    let digest = Sha256::digest(source.as_bytes());
    let id = format!("evt-{}", &hex::encode(digest)[..12]);
    let fingerprint = hex::encode(Sha256::digest(
        format!(
            "{}|{}|{}",
            loop_id.trim(),
            event_type.trim(),
            payload.trim()
        )
        .as_bytes(),
    ));
    let event = LoopQueueEvent {
        id,
        created_at: now_rfc3339(),
        loop_id: loop_id.trim().to_string(),
        event_type: event_type.trim().to_string(),
        status: "queued".to_string(),
        payload: payload.trim().to_string(),
        fingerprint,
    };
    let line = serde_json::to_string(&event)
        .map_err(|e| AgentError::Config(format!("serialize queue event failed: {}", e)))?;
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(loop_queue_path())
        .map_err(|e| AgentError::Io(format!("open queue file failed: {}", e)))?;
    writeln!(file, "{}", line)
        .map_err(|e| AgentError::Io(format!("append queue failed: {}", e)))?;
    Ok(event)
}

fn load_queue_events() -> Result<Vec<LoopQueueEvent>, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let path = loop_queue_path();
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| AgentError::Io(format!("read {} failed: {}", path.display(), e)))?;
    let mut events = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<LoopQueueEvent>(line) {
            events.push(ev);
        }
    }
    Ok(events)
}

fn write_queue_events(events: &[LoopQueueEvent]) -> Result<(), AgentError> {
    let serialized = events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Config(format!("serialize queue events failed: {}", e)))?
        .join("\n");
    let body = if serialized.is_empty() {
        String::new()
    } else {
        format!("{}\n", serialized)
    };
    std::fs::write(loop_queue_path(), body)
        .map_err(|e| AgentError::Io(format!("write queue failed: {}", e)))
}

pub fn recover_orphan_loop_events(max_age_secs: i64) -> Result<usize, AgentError> {
    let mut events = load_queue_events()?;
    let now = Utc::now();
    let mut updated = 0usize;
    for ev in &mut events {
        if ev.status != "running" {
            continue;
        }
        if let Ok(ts) = DateTime::parse_from_rfc3339(&ev.created_at) {
            if (now - ts.with_timezone(&Utc)).num_seconds() > max_age_secs {
                ev.status = "orphaned".to_string();
                updated = updated.saturating_add(1);
            }
        }
    }
    if updated > 0 {
        write_queue_events(&events)?;
    }
    Ok(updated)
}

pub fn replay_loop_queue(limit: usize) -> Result<usize, AgentError> {
    let mut events = load_queue_events()?;
    let mut seen = HashSet::new();
    let mut replayed = 0usize;
    for ev in &mut events {
        if replayed >= limit {
            break;
        }
        if ev.status != "queued" && ev.status != "orphaned" {
            continue;
        }
        if !seen.insert(ev.fingerprint.clone()) {
            ev.status = "deduped".to_string();
            continue;
        }
        ev.status = "replayed".to_string();
        replayed = replayed.saturating_add(1);
    }
    write_queue_events(&events)?;
    Ok(replayed)
}

pub fn refresh_loop_runtime_state(
    loops: &[LoopDefinition],
    background_counts: (usize, usize, usize, usize),
) -> Result<LoopRuntimeState, AgentError> {
    let (queued, running, completed, failed) = background_counts;
    let events = load_queue_events().unwrap_or_default();
    let queue_pending = events.iter().filter(|ev| ev.status == "queued").count();
    let queue_replayable = events
        .iter()
        .filter(|ev| ev.status == "queued" || ev.status == "orphaned")
        .count();
    let orphaned_events = events.iter().filter(|ev| ev.status == "orphaned").count();

    let mut loop_entries = Vec::with_capacity(loops.len());
    for lp in loops {
        let total = completed.saturating_add(failed).max(1);
        let health_score = ((completed as f64) / (total as f64)).clamp(0.0, 1.0);
        let status = if failed > completed {
            "degraded"
        } else if running > 0 {
            "running"
        } else if queued > 0 {
            "queued"
        } else {
            "healthy"
        };
        loop_entries.push(LoopRuntimeEntry {
            id: lp.id.clone(),
            last_status: status.to_string(),
            last_started_at: None,
            last_finished_at: None,
            success_count: completed as u64,
            failure_count: failed as u64,
            health_score,
        });
    }

    let state = LoopRuntimeState {
        updated_at: now_rfc3339(),
        loops: loop_entries,
        queue_pending,
        queue_replayable,
        orphaned_events,
    };
    write_json_file(&loop_runtime_path(), &state)?;
    Ok(state)
}

pub async fn contextlattice_status() -> ContextLatticeStatus {
    let base_url = std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|_| std::env::var("MEMMCP_ORCHESTRATOR_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string())
        .trim_end_matches('/')
        .to_string();
    let health_line = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => match client.get(format!("{base_url}/health")).send().await {
            Ok(resp) if resp.status().is_success() => {
                format!("contextlattice: healthy ({base_url})")
            }
            Ok(resp) => format!("contextlattice: unhealthy (status {})", resp.status()),
            Err(err) => format!("contextlattice: unreachable ({})", err),
        },
        Err(err) => format!("contextlattice: client_error ({})", err),
    };

    let preflight_line = format!(
        "contextlattice preflight: Rust-native memory write endpoint {base_url}/memory/write"
    );

    ContextLatticeStatus {
        health_line,
        preflight_line,
    }
}

pub async fn provider_router_snapshot(limit: usize) -> Vec<ProviderRouteCandidate> {
    let mut rows = Vec::new();
    let client = default_client();
    client.fetch(false).await;
    let providers = curated_provider_slugs();

    for provider in providers {
        let models = provider_model_ids(provider).await;
        for model_id in models.into_iter().take(3) {
            let provider_model = format!("{}:{}", provider, model_id);
            let info = get_model_info(&provider_model).or_else(|| get_model_info(&model_id));
            let supports_tools = info.as_ref().map(|i| i.supports_tools).unwrap_or(true);
            let supports_reasoning = info.as_ref().map(|i| i.supports_reasoning).unwrap_or(false);
            let context_window = get_model_context_length(&provider_model);
            let mut score = 0.0f64;
            if supports_tools {
                score += 1.0;
            }
            if supports_reasoning {
                score += 1.0;
            }
            if context_window >= 128_000 {
                score += 1.0;
            }
            if provider.eq_ignore_ascii_case("nous") {
                score += 0.25;
            }
            rows.push(ProviderRouteCandidate {
                provider: provider.to_string(),
                model: model_id,
                score,
                supports_tools,
                supports_reasoning,
                context_window,
            });
        }
    }

    rows.sort_by(|a, b| b.score.total_cmp(&a.score));
    rows.truncate(limit.max(1));
    rows
}

pub fn recommend_reasoning_level_from_text(text: &str) -> &'static str {
    let lowered = text.to_ascii_lowercase();
    if [
        "security",
        "production",
        "money",
        "trading",
        "risk",
        "parity",
        "release",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        "xhigh"
    } else if ["debug", "implement", "architecture", "investigate"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        "high"
    } else if ["summarize", "quick", "short", "list"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        "low"
    } else {
        "medium"
    }
}

pub fn oauth_session_sentinel() -> Vec<OAuthTokenStatus> {
    let mut out = Vec::new();
    let path = hermes_config::hermes_home()
        .join("auth")
        .join("tokens.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return out;
    };

    let mut push_status = |provider: String, expires_at: String| {
        let status = DateTime::parse_from_rfc3339(&expires_at)
            .map(|ts| {
                let secs = (ts.with_timezone(&Utc) - Utc::now()).num_seconds();
                if secs < 0 {
                    "expired"
                } else if secs < 3600 {
                    "expires<1h"
                } else if secs < 86_400 {
                    "expires<24h"
                } else {
                    "ok"
                }
            })
            .unwrap_or("unknown");
        out.push(OAuthTokenStatus {
            provider,
            expires_at,
            status: status.to_string(),
        });
    };

    if let Some(obj) = value.as_object() {
        if let Some(creds) = obj.get("credentials").and_then(|v| v.as_array()) {
            for cred in creds {
                let provider = cred
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let expires = cred
                    .get("expires_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !expires.is_empty() {
                    push_status(provider, expires);
                }
            }
        } else {
            for (provider, entry) in obj {
                let expires = entry
                    .get("expires_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !expires.is_empty() {
                    push_status(provider.to_string(), expires);
                }
            }
        }
    }

    out.sort_by(|a, b| a.provider.cmp(&b.provider));
    out
}

pub fn summarize_objective_contract(contract: &ObjectiveContract) -> String {
    let terms = contract
        .utility
        .terms
        .iter()
        .map(|t| format!("{}:{:.2}", t.name, t.weight))
        .collect::<Vec<_>>()
        .join(", ");
    let constraints = contract
        .utility
        .hard_constraints
        .iter()
        .map(|c| c.expression.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "objective_id: {}\nlifecycle_status: {}\nbehavior_mode: {}\nwait_barrier: {}\nconfidence: {:.2}\nutility_terms: {}\nhard_constraints: {}\nhorizons: {}",
        contract.id,
        canonical_objective_lifecycle_status(&contract.lifecycle_status),
        canonical_objective_behavior_mode(&contract.behavior_mode),
        summarize_objective_wait_barrier(contract),
        contract.confidence,
        terms,
        constraints,
        contract
            .horizons
            .iter()
            .map(|h| h.horizon.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub async fn render_mission_board(
    current_model: &str,
    session_objective: Option<&str>,
    background_counts: (usize, usize, usize, usize),
) -> Result<String, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let loops = load_alpha_loops()?;
    let runtime = refresh_loop_runtime_state(&loops, background_counts)?;
    let objective = load_objective_contract()?;
    let subagents = load_subagent_registry()?;
    let ctx_policy = load_contextlattice_policy()?;
    let ctx_status = contextlattice_status().await;
    let provider_rows = provider_router_snapshot(6).await;
    let oauth_rows = oauth_session_sentinel();

    let mut out = String::new();
    out.push_str("Mission Control\n\n");
    out.push_str(&format!(
        "session_objective: {}\n",
        session_objective.unwrap_or("(none; use /objective <text>)")
    ));
    out.push_str(&format!("model: {}\n", current_model));
    out.push_str(&format!(
        "reasoning_policy_recommendation: {}\n\n",
        recommend_reasoning_level_from_text(session_objective.unwrap_or(current_model),)
    ));

    out.push_str("ContextLattice\n");
    out.push_str(&format!("- {}\n", ctx_status.health_line));
    out.push_str(&format!("- {}\n", ctx_status.preflight_line));
    out.push_str(&format!(
        "- policy: preflight={} context_pack_on_start={} degradation_aware={} readback_required={}\n",
        ctx_policy.preflight_required,
        ctx_policy.auto_context_pack_on_mission_start,
        ctx_policy.degradation_aware_planning,
        ctx_policy.readback_verification_required
    ));
    out.push_str(&format!(
        "- retrieval: mode={} grounding_required={} retrieval_debug={} broaden_scope={} recency_pass={}\n",
        ctx_policy.preferred_retrieval_mode,
        ctx_policy.include_grounding_required,
        ctx_policy.include_retrieval_debug_for_execution,
        ctx_policy.broaden_scope_on_zero_hits,
        ctx_policy.scoped_recency_pass_before_finalize
    ));
    out.push_str(&format!(
        "- integrity: contradiction_check={} numeric_verbatim={} project_scoping={} checkpoint_payload_contract={}\n",
        ctx_policy.contradiction_check_across_layers,
        ctx_policy.numeric_fact_verbatim_copy,
        ctx_policy.required_project_scoping,
        ctx_policy.checkpoint_payload_requires_project_file_topic
    ));
    out.push_str(&format!(
        "- retries: deep={:?} regular={:?} sinks={}\n\n",
        ctx_policy.deep_retry_budget_secs,
        ctx_policy.regular_retry_budget_secs,
        ctx_policy.summary_sink_order.join(",")
    ));

    out.push_str("Objective Contract\n");
    if let Some(contract) = objective {
        out.push_str(&format!("- updated_at: {}\n", contract.updated_at));
        out.push_str(&format!(
            "- {}\n",
            summarize_objective_contract(&contract).replace('\n', " | ")
        ));
    } else {
        out.push_str("- no persisted objective contract yet\n");
    }
    out.push('\n');

    out.push_str("Subagent Runtime\n");
    out.push_str(&format!(
        "- deterministic_lineage={} durable_checkpoints={} contradiction_detection={}\n",
        subagents.deterministic_lineage,
        subagents.durable_checkpoints,
        subagents.contradiction_detection
    ));
    out.push_str(&format!(
        "- profiles={} (skill-affinity registry active)\n\n",
        subagents.profiles.len()
    ));

    out.push_str("Loop Runtime\n");
    out.push_str(&format!(
        "- loops={} queue_pending={} replayable={} orphaned={}\n",
        runtime.loops.len(),
        runtime.queue_pending,
        runtime.queue_replayable,
        runtime.orphaned_events
    ));
    for row in runtime.loops.iter().take(8) {
        out.push_str(&format!(
            "  - {} status={} health={:.2} success={} failure={}\n",
            row.id, row.last_status, row.health_score, row.success_count, row.failure_count
        ));
    }
    out.push('\n');

    out.push_str("Provider Intelligence (top candidates)\n");
    for row in provider_rows {
        out.push_str(&format!(
            "- {}:{} score={:.2} tools={} reasoning={} ctx={}\n",
            row.provider,
            row.model,
            row.score,
            row.supports_tools,
            row.supports_reasoning,
            row.context_window
        ));
    }
    out.push('\n');

    out.push_str("OAuth Sentinel\n");
    if oauth_rows.is_empty() {
        out.push_str("- no token expiry metadata detected\n");
    } else {
        for row in oauth_rows {
            out.push_str(&format!(
                "- provider={} expires_at={} status={}\n",
                row.provider, row.expires_at, row.status
            ));
        }
    }
    Ok(out)
}

pub fn utility_terms_from_contract() -> Result<HashMap<String, f64>, AgentError> {
    let Some(contract) = load_objective_contract()? else {
        return Ok(HashMap::new());
    };
    Ok(contract
        .utility
        .terms
        .iter()
        .map(|term| (term.name.clone(), term.weight))
        .collect())
}
