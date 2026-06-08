//! Route learning for smart model routing — online learning of cheap-route reliability.
//!
//! Extracted from `impl AgentLoop` in `agent_loop.rs` to reduce the God struct.
//! All functions take `agent: &AgentLoop` instead of `&self`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use hermes_core::Message;

use crate::agent_config::AgentConfig;
use crate::agent_loop::{AgentLoop, TurnRuntimeRoute};
use crate::credential_pool::CredentialPool;
use crate::replay::{RouteLearningState, RouteLearningStats};
use crate::smart_model_routing::{
    ApiMode, CheapModelRouteConfig, PrimaryRuntime, ResolveTurnOutcome, ResolvedCheapRuntime,
    TurnRouteSignature, detect_api_mode_for_url, resolve_turn_route,
};

// ---------------------------------------------------------------------------
// Smart routing learning configuration helpers
// ---------------------------------------------------------------------------

pub(crate) fn smart_routing_learning_enabled() -> bool {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_ENABLED")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(true)
}

pub(crate) fn smart_routing_learning_alpha() -> f64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_ALPHA")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.01..=1.0).contains(v))
        .unwrap_or(0.20)
}

pub(crate) fn smart_routing_learning_cheap_bias() -> f64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (-0.50..=0.50).contains(v))
        .unwrap_or(0.08)
}

pub(crate) fn smart_routing_learning_switch_margin() -> f64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=0.50).contains(v))
        .unwrap_or(0.03)
}

pub(crate) fn smart_routing_learning_ttl_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(7 * 24 * 60 * 60)
}

pub(crate) fn smart_routing_learning_half_life_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(24 * 60 * 60)
}

pub(crate) fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------
// State persistence
// ---------------------------------------------------------------------------

pub(crate) fn default_route_learning_home(config: &AgentConfig) -> PathBuf {
    config
        .hermes_home
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home)
}

pub(crate) fn route_learning_state_path(config: &AgentConfig) -> PathBuf {
    default_route_learning_home(config)
        .join("logs")
        .join("route-learning.json")
}

pub(crate) fn load_route_learning_state(
    config: &AgentConfig,
) -> HashMap<String, RouteLearningStats> {
    if !smart_routing_learning_enabled() {
        return HashMap::new();
    }
    let path = route_learning_state_path(config);
    let raw = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return HashMap::new(),
    };
    let parsed: RouteLearningState = match serde_json::from_str(&raw) {
        Ok(state) => state,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "failed to parse route-learning state; starting empty"
            );
            return HashMap::new();
        }
    };
    let mut entries = parsed.entries;
    let now_ms = now_unix_ms();
    let _ = prune_route_learning_locked(&mut entries, now_ms);
    entries
}

pub(crate) fn save_route_learning_state(
    agent: &AgentLoop,
    entries: &HashMap<String, RouteLearningStats>,
) {
    let path = route_learning_state_path(&agent.config());
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                path = %parent.display(),
                error = %err,
                "failed to create route-learning state directory"
            );
            return;
        }
    }
    let body = RouteLearningState {
        schema_version: 1,
        saved_at_unix_ms: now_unix_ms(),
        entries: entries.clone(),
    };
    let serialized = match serde_json::to_vec_pretty(&body) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "failed to serialize route-learning state");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(err) = std::fs::write(&tmp, serialized) {
        tracing::warn!(
            path = %tmp.display(),
            error = %err,
            "failed to write route-learning state temp file"
        );
        return;
    }
    if let Err(err) = std::fs::rename(&tmp, &path) {
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "failed to move route-learning state into place"
        );
    }
}

// ---------------------------------------------------------------------------
// Effective stats with decay
// ---------------------------------------------------------------------------

pub(crate) fn route_learning_effective_stats(
    stats: &RouteLearningStats,
    now_ms: i64,
) -> Option<RouteLearningStats> {
    if stats.samples == 0 {
        return None;
    }
    let mut out = stats.clone();
    if out.updated_at_unix_ms <= 0 {
        return Some(out);
    }
    let age_ms = now_ms.saturating_sub(out.updated_at_unix_ms).max(0);
    let ttl_secs = smart_routing_learning_ttl_secs();
    if ttl_secs > 0 {
        let ttl_ms = ttl_secs.saturating_mul(1000);
        if age_ms >= ttl_ms {
            return None;
        }
    }
    let half_life_secs = smart_routing_learning_half_life_secs();
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

pub(crate) fn prune_route_learning_locked(
    map: &mut HashMap<String, RouteLearningStats>,
    now_ms: i64,
) -> bool {
    let before = map.len();
    map.retain(|_, stats| route_learning_effective_stats(stats, now_ms).is_some());
    map.len() != before
}

// ---------------------------------------------------------------------------
// Key helpers
// ---------------------------------------------------------------------------

pub(crate) fn extract_provider_and_model<'a>(
    agent: &AgentLoop,
    model: &'a str,
) -> (String, &'a str) {
    if let Some((p, m)) = model.split_once(':') {
        let p = p.trim();
        let m = m.trim();
        if !p.is_empty() && !m.is_empty() {
            return (p.to_string(), m);
        }
    }
    let fallback_provider = agent
        .config()
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("openai")
        .to_string();
    (fallback_provider, model)
}

pub(crate) fn route_learning_key(
    agent: &AgentLoop,
    provider_hint: Option<&str>,
    model: &str,
) -> String {
    let (inferred_provider, inferred_model) = extract_provider_and_model(agent, model);
    let provider = provider_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(inferred_provider.as_str())
        .to_ascii_lowercase();
    format!(
        "{}:{}",
        provider,
        inferred_model.trim().to_ascii_lowercase()
    )
}

pub(crate) fn route_learning_key_for_route(
    agent: &AgentLoop,
    route: Option<&TurnRuntimeRoute>,
    response_model: Option<&str>,
) -> String {
    if let Some(model) = response_model.map(str::trim).filter(|s| !s.is_empty()) {
        let provider_hint = route.and_then(|r| r.provider.as_deref());
        return route_learning_key(agent, provider_hint, model);
    }
    if let Some(rt) = route {
        return route_learning_key(agent, rt.provider.as_deref(), rt.model.as_str());
    }
    let snap = primary_runtime_snapshot(agent);
    route_learning_key(agent, snap.provider.as_deref(), snap.model.as_str())
}

pub(crate) fn route_learning_stats_for_key(
    agent: &AgentLoop,
    key: &str,
) -> Option<RouteLearningStats> {
    let now_ms = now_unix_ms();
    let mut persist_snapshot: Option<HashMap<String, RouteLearningStats>> = None;
    let stats = if let Ok(mut map) = agent.route_learning.lock() {
        let mut changed = prune_route_learning_locked(&mut map, now_ms);
        let out = map
            .get(key)
            .and_then(|stats| route_learning_effective_stats(stats, now_ms));
        if out.is_none() && map.remove(key).is_some() {
            changed = true;
        }
        if changed {
            persist_snapshot = Some(map.clone());
        }
        out
    } else {
        None
    };
    if let Some(snapshot) = persist_snapshot {
        save_route_learning_state(agent, &snapshot);
    }
    stats
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

pub(crate) fn route_learning_score(
    stats: Option<&RouteLearningStats>,
    cheap_bias: f64,
) -> f64 {
    let success_rate = stats.map(|s| s.success_rate).unwrap_or(0.90);
    let avg_latency_ms = stats.map(|s| s.avg_latency_ms).unwrap_or(1800.0);
    let latency_score = (1.0 / (1.0 + (avg_latency_ms / 2500.0))).clamp(0.05, 1.0);
    let failure_penalty = stats
        .map(|s| (s.consecutive_failures as f64 * 0.08).min(0.35))
        .unwrap_or(0.0);
    let exploration_bonus = stats
        .map(|s| {
            let coverage = (s.samples.min(20) as f64) / 20.0;
            (1.0 - coverage) * 0.03
        })
        .unwrap_or(0.03);
    (success_rate * 0.60) + (latency_score * 0.30) + cheap_bias + exploration_bonus
        - failure_penalty
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

pub(crate) fn update_route_learning(
    agent: &AgentLoop,
    route: Option<&TurnRuntimeRoute>,
    response_model: Option<&str>,
    latency_ms: u64,
    success: bool,
) {
    if !smart_routing_learning_enabled() {
        return;
    }
    let key = route_learning_key_for_route(agent, route, response_model);
    let alpha = smart_routing_learning_alpha();
    let mut persist_snapshot: Option<HashMap<String, RouteLearningStats>> = None;
    if let Ok(mut map) = agent.route_learning.lock() {
        let now_ms = now_unix_ms();
        let _ = prune_route_learning_locked(&mut map, now_ms);
        let entry = map.entry(key).or_insert_with(RouteLearningStats::default);
        entry.samples = entry.samples.saturating_add(1);
        if entry.samples == 1 {
            entry.success_rate = if success { 1.0 } else { 0.0 };
            entry.avg_latency_ms = latency_ms as f64;
        } else {
            let observed_success = if success { 1.0 } else { 0.0 };
            entry.success_rate = (1.0 - alpha) * entry.success_rate + alpha * observed_success;
            entry.avg_latency_ms =
                (1.0 - alpha) * entry.avg_latency_ms + alpha * (latency_ms as f64);
        }
        if success {
            entry.consecutive_failures = 0;
        } else {
            entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        }
        entry.updated_at_unix_ms = now_ms;
        persist_snapshot = Some(map.clone());
    }
    if let Some(snapshot) = persist_snapshot {
        save_route_learning_state(agent, &snapshot);
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

pub(crate) fn route_learning_snapshot(
    agent: &AgentLoop,
    route: Option<&TurnRuntimeRoute>,
    response_model: Option<&str>,
) -> Value {
    let key = route_learning_key_for_route(agent, route, response_model);
    let stats = route_learning_stats_for_key(agent, &key);
    let score = route_learning_score(stats.as_ref(), 0.0);
    let ttl_secs = smart_routing_learning_ttl_secs();
    let half_life_secs = smart_routing_learning_half_life_secs();
    serde_json::json!({
        "key": key,
        "enabled": smart_routing_learning_enabled(),
        "ttl_secs": ttl_secs,
        "half_life_secs": half_life_secs,
        "score": score,
        "stats": stats,
    })
}

// ---------------------------------------------------------------------------
// Latest user text
// ---------------------------------------------------------------------------

pub(crate) fn latest_user_text<'a>(messages: &'a [Message]) -> Option<&'a str> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Primary runtime snapshot
// ---------------------------------------------------------------------------

pub(crate) fn primary_runtime_snapshot(agent: &AgentLoop) -> PrimaryRuntime {
    let mut snap = agent
        .state
        .lock()
        .map(|state| state.active_runtime.clone())
        .unwrap_or_else(|_| agent.stored_primary_runtime.clone());
    snap.credential_pool = agent.primary_credential_pool.clone();
    snap
}

// ---------------------------------------------------------------------------
// Resolve smart runtime route
// ---------------------------------------------------------------------------

fn try_build_cheap_runtime(
    agent: &AgentLoop,
    cheap: &CheapModelRouteConfig,
    explicit_api_key: Option<String>,
) -> Result<ResolvedCheapRuntime, ()> {
    let provider_raw = cheap.provider.as_deref().map(str::trim).unwrap_or("");
    if provider_raw.is_empty() {
        return Err(());
    }
    let provider_lc = provider_raw.to_lowercase();
    let model_full = cheap.model.as_deref().map(str::trim).unwrap_or("");
    if model_full.is_empty() {
        return Err(());
    }
    let (_, model_name) = extract_provider_and_model(agent, model_full);
    let base_url =
        crate::runtime_provider::resolve_runtime_base_url(agent, &provider_lc, cheap.base_url.as_deref());
    let api_mode = base_url
        .as_deref()
        .and_then(detect_api_mode_for_url)
        .unwrap_or(ApiMode::ChatCompletions);

    let has_runtime_override = explicit_api_key
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .is_some()
        || cheap
            .base_url
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let pool_ref = if has_runtime_override {
        None
    } else {
        agent.primary_credential_pool.as_ref()
    };

    crate::runtime_provider::build_runtime_provider(
        agent,
        &provider_lc,
        model_name,
        cheap.base_url.as_deref(),
        cheap.api_key_env.as_deref(),
        explicit_api_key.as_deref(),
        Some(&api_mode),
        pool_ref,
    )
    .map_err(|_| ())?;

    let (command, args) = crate::runtime_provider::resolve_runtime_command_args(agent, Some(&provider_lc));
    if provider_lc == "copilot-acp"
        && command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        && !base_url
            .as_deref()
            .map(|u| u.starts_with("acp+tcp://"))
            .unwrap_or(false)
    {
        return Err(());
    }
    if provider_lc == "copilot-acp"
        && !base_url
            .as_deref()
            .map(|u| u.starts_with("acp+tcp://"))
            .unwrap_or(false)
    {
        if let Some(cmd) = command.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if which::which(cmd).is_err() {
                return Err(());
            }
        }
    }
    Ok(ResolvedCheapRuntime {
        model: model_full.to_string(),
        provider: provider_lc,
        base_url,
        api_mode,
        command,
        args,
        credential_pool: if has_runtime_override {
            None
        } else {
            agent.primary_credential_pool.clone()
        },
        skip_primary_credential_pool_fallback: has_runtime_override,
    })
}

pub(crate) fn resolve_smart_runtime_route(
    agent: &AgentLoop,
    messages: &[Message],
) -> Option<TurnRuntimeRoute> {
    let text = latest_user_text(messages)?;
    let primary = primary_runtime_snapshot(agent);
    let outcome = resolve_turn_route(
        text,
        &agent.config().smart_model_routing,
        &primary,
        |cheap, explicit_key| try_build_cheap_runtime(agent, cheap, explicit_key),
    );

    match outcome {
        ResolveTurnOutcome::CheapRouted {
            model,
            label,
            runtime,
            signature,
        } => {
            let primary_key =
                route_learning_key(agent, primary.provider.as_deref(), primary.model.as_str());
            let cheap_key = route_learning_key(
                agent,
                Some(runtime.provider.as_str()),
                runtime.model.as_str(),
            );
            let primary_stats = route_learning_stats_for_key(agent, &primary_key);
            let cheap_stats = route_learning_stats_for_key(agent, &cheap_key);
            let primary_score = route_learning_score(primary_stats.as_ref(), 0.0);
            let cheap_score = route_learning_score(
                cheap_stats.as_ref(),
                smart_routing_learning_cheap_bias(),
            );
            let margin = smart_routing_learning_switch_margin();
            if smart_routing_learning_enabled() && (cheap_score + margin) < primary_score {
                tracing::debug!(
                    primary_key = %primary_key,
                    cheap_key = %cheap_key,
                    primary_score,
                    cheap_score,
                    margin,
                    "smart routing online-learning selected primary route"
                );
                return None;
            }
            let cheap = agent.config().smart_model_routing.cheap_model.clone()?;
            Some(TurnRuntimeRoute {
                model,
                provider: Some(runtime.provider.clone()),
                base_url: runtime.base_url.clone(),
                api_key_env: cheap
                    .api_key_env
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                api_mode: Some(runtime.api_mode.clone()),
                command: runtime.command.clone(),
                args: runtime.args.clone(),
                credential_pool: runtime.credential_pool.clone(),
                credential_pool_fallback: !runtime.skip_primary_credential_pool_fallback,
                route_label: Some(format!(
                    "{} [cheap_score={:.3} primary_score={:.3}]",
                    label, cheap_score, primary_score
                )),
                routing_reason: Some("simple_turn_online_learning".to_string()),
                signature,
            })
        }
        ResolveTurnOutcome::Primary { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Cost degrade model
// ---------------------------------------------------------------------------

pub(crate) fn resolve_cost_degrade_model(agent: &AgentLoop) -> Option<String> {
    if let Some(ref m) = agent.config().cost_guard_degrade_model {
        if !m.trim().is_empty() {
            return Some(m.trim().to_string());
        }
    }
    if let Some(ref m) = agent.config().retry.fallback_model {
        if !m.trim().is_empty() {
            return Some(m.trim().to_string());
        }
    }
    if crate::runtime_provider::active_model(agent).trim() != "openai:gpt-4o-mini" {
        return Some("openai:gpt-4o-mini".to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// Reliability degrade model
// ---------------------------------------------------------------------------

pub(crate) fn resolve_reliability_degrade_model(
    agent: &AgentLoop,
    active_model: &str,
    route: Option<&TurnRuntimeRoute>,
) -> Option<String> {
    if let Some(ref fallback) = agent.config().retry.fallback_model {
        if !fallback.trim().is_empty() && !fallback.eq_ignore_ascii_case(active_model) {
            return Some(fallback.trim().to_string());
        }
    }
    let config_provider = agent.config().provider.clone();
    let provider_hint = route
        .and_then(|r| r.provider.as_deref())
        .or(config_provider.as_deref())
        .unwrap_or("openai");
    let (_, active_model_id) = extract_provider_and_model(agent, active_model);
    if let Some(candidate) =
        crate::agent_loop::preferred_tool_payload_fallback_model(provider_hint, active_model_id)
    {
        let normalized = if candidate.contains(':') {
            candidate
        } else {
            format!("{}:{}", provider_hint, candidate)
        };
        if !normalized.eq_ignore_ascii_case(active_model) {
            return Some(normalized);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Retry failover chain
// ---------------------------------------------------------------------------

pub(crate) fn resolve_retry_failover_chain(
    agent: &AgentLoop,
    active_model: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let active_lc = active_model.trim().to_ascii_lowercase();

    let mut push_candidate = |candidate: &str| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            return;
        }
        let normalized = trimmed.to_ascii_lowercase();
        if normalized == active_lc {
            return;
        }
        if seen.insert(normalized) {
            out.push(trimmed.to_string());
        }
    };

    for model in &agent.config().retry.fallback_models {
        push_candidate(model);
    }
    if let Some(ref fallback) = agent.config().retry.fallback_model {
        push_candidate(fallback);
    }
    if let Some(dynamic) = resolve_reliability_degrade_model(agent, active_model, None) {
        push_candidate(&dynamic);
    }

    out
}

// ---------------------------------------------------------------------------
// Turn route cost guard
// ---------------------------------------------------------------------------

pub(crate) fn turn_route_cost_guard(agent: &AgentLoop, model: String) -> TurnRuntimeRoute {
    let pri = primary_runtime_snapshot(agent);
    let mut sig = pri.to_signature();
    sig.model = model.clone();
    TurnRuntimeRoute {
        model,
        provider: None,
        base_url: None,
        api_key_env: None,
        api_mode: None,
        command: None,
        args: Vec::new(),
        credential_pool: agent.primary_credential_pool.clone(),
        credential_pool_fallback: true,
        route_label: None,
        routing_reason: Some("cost_guard".to_string()),
        signature: sig,
    }
}

// ---------------------------------------------------------------------------
// Turn route reliability guard
// ---------------------------------------------------------------------------

pub(crate) fn turn_route_reliability_guard(agent: &AgentLoop, model: String) -> TurnRuntimeRoute {
    let pri = primary_runtime_snapshot(agent);
    let (provider, _) = extract_provider_and_model(agent, model.as_str());
    let mut sig = pri.to_signature();
    sig.model = model.clone();
    TurnRuntimeRoute {
        model,
        provider: Some(provider),
        base_url: None,
        api_key_env: None,
        api_mode: None,
        command: None,
        args: Vec::new(),
        credential_pool: agent.primary_credential_pool.clone(),
        credential_pool_fallback: true,
        route_label: None,
        routing_reason: Some("reliability_guard".to_string()),
        signature: sig,
    }
}
